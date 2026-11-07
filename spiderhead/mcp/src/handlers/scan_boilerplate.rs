//! `boilerplate_report`: group a retained corpus by header/nav/footer hash and
//! report minority template groups; never issues a request. Pages whose bodies
//! were not retained are counted, not guessed from.

use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use spiderhead_store::bodies::read_body;
use spiderhead_store::scan_artifact::open_scan_mode;

use crate::error::ToolError;

use super::kwargs;

const FORMAT: &str = "spiderhead.boilerplate-report.v1";
/// Per-page body decode budget; a whole retained corpus fits many times over.
const DEFAULT_MAX_BYTES: i64 = 8 * 1024 * 1024;
/// Sample URLs listed per group.
const DEFAULT_SAMPLE: i64 = 5;

fn refused(message: &str) -> Value {
    json!({
        "ok": false,
        "state": "invalid",
        "format": FORMAT,
        "reason_code": "invalid_boilerplate_report",
        "error": message,
    })
}

/// The hash of one element's serialized markup, or an empty string when the
/// page has no such element: pages without a header still group together.
fn element_hash(html: &str, selector: fn(&str) -> Option<String>) -> String {
    match selector(html) {
        Some(markup) => {
            let digest = Sha256::digest(markup.as_bytes());
            digest.iter().map(|b| format!("{b:02x}")).collect()
        }
        None => String::new(),
    }
}

/// The first `<header>…</header>` span, tags included, case-insensitively.
/// A plain span scan is deliberate: template chrome is flat markup, and a DOM
/// parse of thousands of retained bodies would dwarf the report.
fn first_span(html: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}");
    let lower = html.to_ascii_lowercase();
    let mut from = 0usize;
    let start = loop {
        let at = lower[from..].find(&open)? + from;
        let after = lower[at + open.len()..].chars().next();
        if matches!(
            after,
            Some(' ') | Some('>') | Some('/') | Some('\n') | Some('\t') | None
        ) {
            break at;
        }
        from = at + open.len();
    };
    let end = loop {
        let at = lower[from..].find(&close)? + from;
        let after = lower[at + close.len()..].chars().next();
        if matches!(after, Some('>')) {
            break at + close.len() + 1;
        }
        from = at + close.len();
    };
    Some(html[start..end.min(html.len())].to_owned())
}

/// The last `<footer>…</footer>` span: page chrome's closing block.
fn last_footer(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let close = "</footer";
    let mut until = lower.len();
    let end = loop {
        let at = lower[..until].rfind(close)?;
        let after = lower[at + close.len()..].chars().next();
        if matches!(after, Some('>')) {
            break at + close.len() + 1;
        }
        until = at;
    };
    let open_at = lower[..end].rfind("<footer")?;
    Some(html[open_at..end.min(html.len())].to_owned())
}

fn header_of(html: &str) -> Option<String> {
    first_span(html, "header")
}

fn nav_of(html: &str) -> Option<String> {
    first_span(html, "nav")
}

/// `boilerplate_report`: hash every retained page's header/nav/footer chrome
/// and group pages by that triple. The largest group is the site template;
/// minority groups are pages whose chrome differs — hand-built pages, broken
/// layouts or split templates.
pub fn boilerplate_report(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    // The CLI flag's dest is `scan`, the MCP argument is `input_path`.
    let Some(input) = args
        .get("input_path")
        .or_else(|| args.get("scan"))
        .and_then(Value::as_str)
    else {
        return Ok(refused(
            "scan is required: input_path must name a retained scan SQLite file",
        ));
    };
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_MAX_BYTES)
        .max(1);
    let sample = args
        .get("sample")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_SAMPLE)
        .clamp(0, 50) as usize;
    let (con, _validation) = match open_scan_mode(Path::new(input), false, None, true) {
        Ok(opened) => opened,
        Err(error) => return Ok(refused(&format!("cannot open scan: {error}"))),
    };
    let pages_total: i64 = con
        .query_row("SELECT COUNT(*) FROM pages", [], |row| row.get(0))
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let mut statement = con
        .prepare(
            "SELECT u.url, d.body_sha256 FROM documents d JOIN urls u USING(url_id) \
             WHERE d.representation='static' AND d.body_sha256 IS NOT NULL \
             ORDER BY u.url_id",
        )
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let listed = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| ToolError::Failed(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ToolError::Failed(error.to_string()))?;

    let mut groups: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut urls_by_group: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut decoded = 0i64;
    let mut undecodable = 0i64;
    for (url, sha) in &listed {
        let body = match read_body(&con, sha, max_bytes) {
            Ok(body) => body,
            Err(_) => {
                undecodable += 1;
                continue;
            }
        };
        decoded += 1;
        let html = String::from_utf8_lossy(&body);
        let key = format!(
            "{}|{}|{}",
            element_hash(&html, header_of),
            element_hash(&html, nav_of),
            element_hash(&html, last_footer)
        );
        *groups.entry(key.clone()).or_insert(0) += 1;
        urls_by_group.entry(key).or_default().push(url.clone());
    }

    let mut ranked: Vec<(String, i64)> = groups.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let largest = ranked.first().map(|(_, n)| *n).unwrap_or(0);
    let groups_out: Vec<Value> = ranked
        .iter()
        .map(|(key, count)| {
            let mut parts = key.splitn(3, '|');
            json!({
                "header_sha256": parts.next().unwrap_or_default(),
                "nav_sha256": parts.next().unwrap_or_default(),
                "footer_sha256": parts.next().unwrap_or_default(),
                "pages": count,
                "share": if decoded > 0 {
                    (*count as f64 / decoded as f64 * 1000.0).round() / 10.0
                } else {
                    0.0
                },
                "template": count == &largest && largest > 0,
                "sample_urls": urls_by_group
                    .get(key)
                    .map(|urls| urls.iter().take(sample).cloned().collect::<Vec<_>>())
                    .unwrap_or_default(),
            })
        })
        .collect();
    let minority: i64 = ranked.iter().skip(1).map(|(_, count)| *count).sum();
    Ok(json!({
        "ok": true,
        "format": FORMAT,
        "scan": input,
        "pages_total": pages_total,
        "pages_with_bodies": decoded,
        "pages_without_bodies": (pages_total - decoded - undecodable).max(0),
        "bodies_undecodable": undecodable,
        "groups": groups_out.len(),
        "minority_pages": minority,
        "minority_share": if decoded > 0 {
            (minority as f64 / decoded as f64 * 1000.0).round() / 10.0
        } else {
            0.0
        },
        "notes": "pages_without_bodies needs a scan whose bodies were retained (legacy import or body capture); the largest group is the site template, every other group is a minority chrome variant",
        "report": groups_out,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_find_first_header_nav_and_last_footer() {
        let html = "<html><body><header><b>top</b></header><nav><a>1</a></nav>\
                    <main>article</main><footer>f1</footer><footer>f2</footer></body></html>";
        assert!(header_of(html).unwrap().contains("<b>top</b>"));
        assert!(nav_of(html).unwrap().contains("<a>1</a>"));
        assert!(last_footer(html).unwrap().contains("f2"));
        // A page with no chrome groups on three empty hashes.
        assert!(header_of("<p>text</p>").is_none());
    }

    #[test]
    fn header_tag_must_be_the_tag_not_a_prefix() {
        let html = "<html><body><headered>x</headered><header>y</header></body></html>";
        let found = header_of(html).unwrap();
        assert!(found.contains("<header>y</header>"), "{found}");
        assert!(!found.contains("headered"));
    }
}
