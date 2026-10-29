//! Render Blocking Requests (SF PageSpeed): <head> scripts/stylesheets that
//! hold first paint.
//!
//! `check_render_blocking` + `_check_retained_render_blocking` from
//! the reference suite. Only a native seohead crawl
//! measures this (#1022); an SF export without the column skips honestly
//! rather than reporting every page as clean.

use serde_json::{json, Map, Value};
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::pyvalue::str_or_dumps;
use crate::rules::shared::{body_unavailable, con, has_column, rec};

/// One row of the retained-render-blocking page query: page URL, selected
/// static document id (nullable), its body state, final response URL, status.
type RetainedRow = (String, Option<i64>, Option<String>, String, i64);

/// The per-run tally `ctx.asset_measurements["RENDER_BLOCKING"]` carries —
/// keys `unit`/`eligible`/`measured`/`partial`/`unavailable`/`state`/`reason`
/// in this insertion order.
struct Metrics {
    /// Pages the retained store or the column made eligible.
    eligible: i64,
    /// Pages with a usable measurement.
    measured: i64,
    /// Pages whose resource list had to be bounded.
    partial: i64,
    /// Pages with no usable measurement.
    unavailable: i64,
    /// `unavailable`/`partial`/`complete`.
    state: String,
    /// Human-facing note behind a non-complete state.
    reason: String,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            eligible: 0,
            measured: 0,
            partial: 0,
            unavailable: 0,
            // reference seeds the dict with `state: "unavailable"` — it shows
            // through on paths that return before `finalize` runs.
            state: "unavailable".to_owned(),
            reason: String::new(),
        }
    }
}

impl Metrics {
    fn to_json(&self) -> Value {
        json!({
            "unit": "pages",
            "eligible": self.eligible,
            "measured": self.measured,
            "partial": self.partial,
            "unavailable": self.unavailable,
            "state": self.state,
            "reason": self.reason,
        })
    }

    fn finalize(&mut self) {
        self.state = if self.measured == 0 {
            "unavailable".to_owned()
        } else if self.unavailable > 0 || self.partial > 0 {
            "partial".to_owned()
        } else {
            "complete".to_owned()
        };
    }
}

/// `json.dumps(text, ensure_ascii=True)`: every char outside ` -~` becomes
/// `\uXXXX` (surrogate pairs for astral planes), with the usual named escapes.
fn json_dumps_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let n = c as u32;
                if n > 0xffff {
                    let v = n - 0x10000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (v >> 10),
                        0xdc00 + (v & 0x3ff)
                    ));
                } else {
                    out.push_str(&format!("\\u{:04x}", n));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The bounded `sample` list both call paths build: at most 200 URLs and at
/// most 64 KiB of a notional JSON array rendering, `", "` separators included.
fn resource_sample<'a>(urls: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut sample = Vec::new();
    let mut sample_bytes: i64 = 2;
    for url in urls {
        let size = json_dumps_ascii(url).len() as i64 + if sample.is_empty() { 0 } else { 2 };
        if sample.len() < 200 && sample_bytes + size <= 64 * 1024 {
            sample.push(url.to_owned());
            sample_bytes += size;
        }
    }
    sample
}

/// `_check_retained_render_blocking`: read frozen native scan bodies without
/// adding fields to scan.v1.
fn check_retained_render_blocking(
    ctx: &mut AuditContext,
    metrics: &mut Metrics,
) -> Result<(), ContextError> {
    // Same page population AuditContext.html_pages draws on, joined to each
    // page's retained static document where one exists.
    let rows: Vec<RetainedRow> = {
        let mut st = con(ctx)
            .prepare(
                "WITH active_static AS (\
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
                 ORDER BY p.page_ordinal",
            )
            .map_err(|e| ContextError::Value(e.to_string()))?;
        let mapped = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| ContextError::Value(e.to_string()))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ContextError::Value(e.to_string()))?
    };
    for (url, document_id, state, final_url, status) in rows {
        metrics.eligible += 1;
        let Some(document_id) = document_id else {
            metrics.unavailable += 1;
            continue;
        };
        if state.as_deref() != Some("complete") {
            metrics.unavailable += 1;
            continue;
        }
        let resources = {
            let html = match spiderhead_store::bodies::read_document(
                con(ctx),
                document_id,
                2 * 1024 * 1024,
            ) {
                Ok(html) => html,
                Err(_) => {
                    metrics.unavailable += 1;
                    continue;
                }
            };
            let soup = spiderhead_parse::dom::parse_document_lxml(&html);
            let base = document_base_url(DocumentSource::Tree(&soup), &final_url);
            match spiderhead_parse::asset_evidence::find_render_blocking_resources(&soup, &base) {
                Ok(resources) => resources,
                Err(_) => {
                    metrics.unavailable += 1;
                    continue;
                }
            }
        };
        metrics.measured += 1;
        if !resources.is_empty() {
            let urls: Vec<String> = resources.iter().map(|item| item.url.clone()).collect();
            let sample = resource_sample(urls.iter().map(String::as_str));
            if sample.len() < resources.len() {
                metrics.partial += 1;
            }
            let mut details = Map::new();
            details.insert(
                "resources".into(),
                Value::Array(sample.iter().cloned().map(Value::String).collect()),
            );
            details.insert(
                "resources_observed".into(),
                Value::from(resources.len() as i64),
            );
            details.insert(
                "resources_omitted".into(),
                Value::from((resources.len() - sample.len()) as i64),
            );
            details.insert(
                "source".into(),
                Value::String("native-retained-static-head".to_owned()),
            );
            details.insert("source_page".into(), Value::String(url.clone()));
            details.insert(
                "reason".into(),
                Value::String(
                    "retained source head declares blocking scripts or stylesheets; runtime \
                     blocking time was not measured"
                        .to_owned(),
                ),
            );
            ctx.add(
                "RENDER_BLOCKING",
                AddOpts {
                    target_url: Some(url),
                    status_code: Some(status),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    metrics.finalize();
    if metrics.state != "complete" {
        metrics.reason = "not fully measured by this native crawl: static HTML is absent, \
                          incomplete or exceeds the 2 MiB per-document head-analysis budget, or \
                          resource samples were bounded"
            .to_owned();
        ctx.skip("RENDER_BLOCKING", &metrics.reason);
    }
    Ok(())
}

/// `check_render_blocking`.
pub fn check_render_blocking(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let mut metrics = Metrics::default();
    if ctx.scan_con.is_some() {
        check_retained_render_blocking(ctx, &mut metrics)?;
        ctx.asset_measurements
            .insert("RENDER_BLOCKING".to_owned(), metrics.to_json());
        return Ok(());
    }
    if !has_column(ctx, "render_blocking") {
        let count = ctx.html_pages().len() as i64;
        metrics.eligible = count;
        metrics.unavailable = count;
        metrics.reason =
            "no Render Blocking Resources column (needs a native seohead crawl)".to_owned();
        ctx.skip("RENDER_BLOCKING", &metrics.reason);
        ctx.asset_measurements
            .insert("RENDER_BLOCKING".to_owned(), metrics.to_json());
        return Ok(());
    }
    for page in ctx.html_pages() {
        metrics.eligible += 1;
        let (rec_obj, page_url, page_status) = page.map(|p| {
            (
                rec(p).cloned().unwrap_or_default(),
                p.url.clone(),
                p.status_code,
            )
        });
        let value = rec_obj
            .get("render_blocking")
            .cloned()
            .unwrap_or(Value::Null);
        if body_unavailable(&rec_obj) || value.is_null() {
            metrics.unavailable += 1;
            continue;
        }
        // `json.loads(value)` when `str(value)` starts with `[`/`{`, else
        // `str(value).split("\n")`. `str_or_dumps` reproduces `str(value)`
        // for every JSON shape, and parsing the repr of a non-string value
        // fails the same way `json.loads` fails on a non-str in reference.
        let text = str_or_dumps(&value);
        let parsed = if text.starts_with('[') || text.starts_with('{') {
            serde_json::from_str::<Value>(&text)
        } else {
            Ok(Value::Array(
                text.split('\n')
                    .map(|line| Value::String(line.to_owned()))
                    .collect(),
            ))
        };
        let (urls, observed, omitted) = match parsed {
            Ok(Value::Object(payload)) => {
                let (Some(urls), Some(observed), Some(omitted)) = (
                    payload.get("resources"),
                    payload.get("observed"),
                    payload.get("omitted"),
                ) else {
                    metrics.unavailable += 1;
                    continue;
                };
                let Some(urls) = urls.as_array() else {
                    metrics.unavailable += 1;
                    continue;
                };
                let (Some(observed), Some(omitted)) = (observed.as_i64(), omitted.as_i64()) else {
                    metrics.unavailable += 1;
                    continue;
                };
                if omitted < 0 || observed != urls.len() as i64 + omitted {
                    metrics.unavailable += 1;
                    continue;
                }
                (urls.clone(), observed, omitted)
            }
            Ok(payload) => {
                let Some(urls) = payload.as_array() else {
                    metrics.unavailable += 1;
                    continue;
                };
                (urls.clone(), urls.len() as i64, 0)
            }
            Err(_) => {
                metrics.unavailable += 1;
                continue;
            }
        };
        if urls.iter().any(|u| !u.is_string()) {
            metrics.unavailable += 1;
            continue;
        }
        metrics.measured += 1;
        let strings: Vec<String> = urls
            .iter()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect();
        let sample = resource_sample(strings.iter().map(String::as_str));
        let omitted = omitted + (urls.len() - sample.len()) as i64;
        if omitted > 0 {
            metrics.partial += 1;
        }
        if observed > 0 {
            let mut details = Map::new();
            details.insert(
                "resources".into(),
                Value::Array(sample.iter().cloned().map(Value::String).collect()),
            );
            details.insert("resources_omitted".into(), Value::from(omitted));
            details.insert("resources_observed".into(), Value::from(observed));
            details.insert(
                "source".into(),
                Value::String("native-head-markup".to_owned()),
            );
            details.insert("source_page".into(), Value::String(page_url.clone()));
            details.insert(
                "reason".into(),
                Value::String(
                    "head markup declares render-blocking scripts or stylesheets; runtime \
                     blocking time was not measured"
                        .to_owned(),
                ),
            );
            ctx.add(
                "RENDER_BLOCKING",
                AddOpts {
                    target_url: Some(page_url),
                    status_code: page_status,
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    metrics.finalize();
    if metrics.state != "complete" {
        metrics.reason =
            "retained render-blocking markup is absent, unreadable or resource samples were \
             bounded"
                .to_owned();
        ctx.skip("RENDER_BLOCKING", &metrics.reason);
    }
    ctx.asset_measurements
        .insert("RENDER_BLOCKING".to_owned(), metrics.to_json());
    Ok(())
}
