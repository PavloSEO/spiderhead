//! Checks judged from the stored scan itself: response headers and link edges
//! (reference the reference suite, the reference suite, wired in the reference suite).

use std::net::IpAddr;

use serde_json::{json, Map, Value};
use spiderhead_parse::urlutil::hostname;

use super::details;
use crate::audit::AuditContext;

/// Check id -> header whose absence it reports, in registry order.
const HEADER_CHECKS: [(&str, &str); 5] = [
    ("MISSING_HSTS", "strict-transport-security"),
    ("MISSING_CSP", "content-security-policy"),
    ("MISSING_X_CONTENT_TYPE_OPTIONS", "x-content-type-options"),
    ("MISSING_X_FRAME_OPTIONS", "x-frame-options"),
    ("MISSING_REFERRER_POLICY", "referrer-policy"),
];

/// Security headers whose repetition on one response is reported.
const REPEATABLE: [&str; 5] = [
    "strict-transport-security",
    "x-content-type-options",
    "x-frame-options",
    "referrer-policy",
    "permissions-policy",
];

const PAGE_RESPONSES_SQL: &str =
    "SELECT u.url, p.status_code, p.content_type, r.effective_headers_redacted_json \
    FROM pages AS p JOIN urls AS u USING(url_id) \
    LEFT JOIN responses AS r ON r.response_id = (\
      SELECT MAX(response_id) FROM responses WHERE effective_url_id = p.url_id AND purpose = 'page'\
    ) ORDER BY p.page_ordinal";

/// Stored (name, value) pairs in wire order, names lower-cased; `None` when they do not parse.
fn header_pairs(raw: Option<&str>) -> Option<Vec<(String, String)>> {
    let value: Value = serde_json::from_str(raw?).ok()?;
    value
        .as_array()?
        .iter()
        .map(|pair| {
            let pair = pair.as_array().filter(|p| p.len() == 2)?;
            Some((
                pair[0].as_str()?.to_lowercase(),
                pair[1].as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn is_judged_html(status: Option<i64>, content_type: &str) -> bool {
    let media = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    matches!(media.as_str(), "text/html" | "application/xhtml+xml")
        && status.is_some_and(|c| (200..=299).contains(&c))
}

/// `_missing`: absent or blank header; X-Frame-Options is satisfied by CSP `frame-ancestors`.
fn missing(header: &str, map: &std::collections::BTreeMap<String, String>) -> bool {
    if map.get(header).is_some_and(|v| !v.trim().is_empty()) {
        return false;
    }
    if header == "x-frame-options" {
        return !map
            .get("content-security-policy")
            .is_some_and(|v| v.to_lowercase().contains("frame-ancestors"));
    }
    true
}

/// One retained page response judged for security headers, without a request:
/// `{state, reason, headers, findings}` (`measured`, `unavailable` or `not_applicable`).
pub fn evaluate_page(
    url: &str,
    status: Option<i64>,
    content_type: Option<&str>,
    raw_headers: Option<&str>,
) -> Value {
    if !is_judged_html(status, content_type.unwrap_or("")) {
        return json!({
            "state": "not_applicable",
            "reason": "security headers are evaluated on successful HTML page responses",
            "headers": null,
            "findings": [],
        });
    }
    let Some(pairs) = header_pairs(raw_headers) else {
        return json!({
            "state": "unavailable",
            "reason": "response headers were not retained or could not be parsed",
            "headers": null,
            "findings": [],
        });
    };
    let mut headers = Map::new();
    let mut joined: std::collections::BTreeMap<String, String> = Default::default();
    for (name, value) in &pairs {
        let combined = match headers.get(name).and_then(Value::as_str) {
            Some(previous) => format!("{previous}, {value}"),
            None => value.clone(),
        };
        joined.insert(name.clone(), combined.clone());
        headers.insert(name.clone(), json!(combined));
    }
    let mut findings: Vec<Value> = HEADER_CHECKS
        .iter()
        .filter(|(_, header)| missing(header, &joined))
        .map(|(check, header)| json!({"check": check, "target_url": url, "header": header}))
        .collect();
    findings.extend(
        REPEATABLE
            .iter()
            .filter(|h| pairs.iter().filter(|(n, _)| n == *h).count() > 1)
            .map(|header| {
                json!({"check": "DUPLICATE_SECURITY_HEADER", "target_url": url, "header": header})
            }),
    );
    json!({"state": "measured", "reason": "", "headers": headers, "findings": findings})
}

/// Native header checks (#1013) over the final response of every 2xx HTML page.
pub fn check_security_headers(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else {
        for (id, _) in HEADER_CHECKS {
            ctx.skip(
                id,
                "crawl was not stored; per-URL response headers are not retained",
            );
        }
        ctx.skip(
            "DUPLICATE_SECURITY_HEADER",
            "crawl was not stored; per-URL response headers are not retained",
        );
        return;
    };
    let mut found: Vec<Vec<(String, String)>> = vec![Vec::new(); HEADER_CHECKS.len() + 1];
    let (mut measured, mut unmeasured) = (0usize, 0usize);
    if let Ok(mut stmt) = con.prepare(PAGE_RESPONSES_SQL) {
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        });
        for (url, status, content_type, raw) in rows.into_iter().flatten().flatten() {
            if !is_judged_html(status, &content_type) {
                continue;
            }
            let Some(pairs) = header_pairs(raw.as_deref()) else {
                unmeasured += 1;
                continue;
            };
            measured += 1;
            let mut map: std::collections::BTreeMap<String, String> = Default::default();
            for (name, value) in &pairs {
                map.entry(name.clone())
                    .and_modify(|v| *v = format!("{v}, {value}"))
                    .or_insert_with(|| value.clone());
            }
            for (n, (_, header)) in HEADER_CHECKS.iter().enumerate() {
                if missing(header, &map) {
                    found[n].push((url.clone(), (*header).to_owned()));
                }
            }
            for header in REPEATABLE
                .iter()
                .filter(|h| pairs.iter().filter(|(n, _)| n == *h).count() > 1)
            {
                found[HEADER_CHECKS.len()].push((url.clone(), (*header).to_owned()));
            }
        }
    }
    let ids: Vec<&str> = HEADER_CHECKS
        .iter()
        .map(|(id, _)| *id)
        .chain(["DUPLICATE_SECURITY_HEADER"])
        .collect();
    for (id, items) in ids.into_iter().zip(found) {
        for (url, header) in &items {
            ctx.add(
                id,
                Some(url),
                details([("header", json!(header)), ("target_url", json!(url))]),
            );
        }
        if items.is_empty() {
            if unmeasured > 0 {
                ctx.skip(
                    id,
                    &format!("{unmeasured} HTML pages have no parseable stored response headers"),
                );
            } else if measured == 0 {
                ctx.skip(id, "no HTML page with a 2xx response was stored");
            }
        }
    }
    ctx.scan = Some(con);
}

/// `_is_localhost`: `localhost`, `*.localhost` or a literal loopback address.
pub(crate) fn is_localhost(host: &str) -> bool {
    let host = host.to_lowercase();
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_loopback(),
        Ok(IpAddr::V6(a)) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v| v.is_loopback()),
        Err(_) => false,
    }
}

pub(crate) const SELECTED_LINKS: &str = "WITH l AS ( SELECT l.* FROM links l WHERE l.evidence_representation IN ('static','legacy_unknown') \
 UNION ALL SELECT r.* FROM links r JOIN pages p ON p.url_id=r.source_url_id \
 WHERE r.evidence_representation=p.representation AND p.representation IN ('rendered','legacy_fragment') \
 AND NOT EXISTS (SELECT 1 FROM links s WHERE s.source_url_id=r.source_url_id AND s.evidence_representation IN ('static','legacy_unknown') AND s.destination_url_id=r.destination_url_id) \
 AND NOT EXISTS (SELECT 1 FROM links later WHERE later.source_url_id=r.source_url_id AND later.evidence_representation=r.evidence_representation \
 AND later.destination_url_id=r.destination_url_id AND (later.ordinal>r.ordinal OR (later.ordinal=r.ordinal AND later.link_id>r.link_id))) ) ";

/// `link_findings.outlinks_to_localhost`: edges pointing at a loopback address --
/// a dev/staging reference leaked into production markup.
pub(crate) fn outlinks_to_localhost(edges: &[(String, String)]) -> Vec<Map<String, Value>> {
    edges
        .iter()
        .filter(|(_, destination)| {
            hostname(destination)
                .ok()
                .flatten()
                .is_some_and(|h| is_localhost(&h))
        })
        .map(|(source, destination)| {
            details([
                ("target_url", json!(source)),
                ("destination", json!(destination)),
            ])
        })
        .collect()
}

/// `OUTLINK_TO_LOCALHOST`: one finding per link edge that points at a loopback address.
pub fn check_localhost_outlinks(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else { return };
    let sql = format!(
        "{SELECTED_LINKS}SELECT src.url, dst.url FROM l JOIN pages AS sp ON sp.url_id=l.source_url_id \
         JOIN urls AS src ON src.url_id=l.source_url_id JOIN urls AS dst ON dst.url_id=l.destination_url_id \
         ORDER BY sp.page_ordinal, l.ordinal, l.link_id"
    );
    let mut edges: Vec<(String, String)> = Vec::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            edges = rows.flatten().collect();
        }
    }
    for item in outlinks_to_localhost(&edges) {
        let target = item
            .get("target_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        ctx.add("OUTLINK_TO_LOCALHOST", target.as_deref(), item);
    }
    ctx.scan = Some(con);
}

/// The opt-in redirect-hop HSTS probe block of `_audit_crawl_result`: a stored
/// audit re-judges the retained `redirect_hop_probe` context payload instead of
/// probing the site again; with no retained row it reports why it cannot judge
/// rather than claiming a clean result.
pub fn check_redirect_hop_hsts(ctx: &mut AuditContext) {
    use rusqlite::OptionalExtension;
    use spiderhead_store::crawl_records::security_headers::{
        evaluate_redirect_hops, REDIRECT_HOP_PROBE_KIND,
    };

    let retained_probe = ctx.scan.as_ref().and_then(|con| {
        con.query_row(
            "SELECT payload_json FROM context_items WHERE kind=? AND item_key='run'",
            [REDIRECT_HOP_PROBE_KIND],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    });
    let probe_opt_in = super::py_truthy(
        ctx.config
            .get("analysis")
            .and_then(|v| v.get("redirect_hop_hsts_probe")),
    );
    // A list scan keeps a stored start_url the handler never uses as `url`.
    let list_scan = ctx
        .scan
        .as_ref()
        .is_some_and(super::link_predicates::stored_list);
    let url = if list_scan {
        None
    } else {
        ctx.start_url.clone()
    };
    if !probe_opt_in && retained_probe.is_none() {
        ctx.skip(
            "MISSING_HSTS_ON_REDIRECT_HOP",
            "opt-in probe disabled; enable analysis.redirect_hop_hsts_probe",
        );
    } else if url.is_none() {
        ctx.skip(
            "MISSING_HSTS_ON_REDIRECT_HOP",
            "no start URL to derive host variants from",
        );
    } else {
        match retained_probe.as_deref() {
            // Stored audits never probe again: the live crawl that enabled the
            // option retains the observation for reanalysis to re-judge.
            None => ctx.skip(
                "MISSING_HSTS_ON_REDIRECT_HOP",
                "offline reanalysis has no retained redirect-hop probe result",
            ),
            Some(payload) => {
                let probe = serde_json::from_str::<Value>(payload).unwrap_or(Value::Null);
                let verdict = evaluate_redirect_hops(&probe);
                if !verdict.missing.is_empty() {
                    let missing: Vec<Value> = verdict.missing.iter().take(10).cloned().collect();
                    ctx.add(
                        "MISSING_HSTS_ON_REDIRECT_HOP",
                        probe.get("url").and_then(Value::as_str),
                        super::details([
                            ("hops_measured", Value::from(verdict.hops_measured)),
                            ("missing_hops", Value::Array(missing)),
                            (
                                "probe",
                                Value::from(
                                    "host variants of the start URL, per-hop Strict-Transport-Security",
                                ),
                            ),
                        ]),
                    );
                } else if verdict.state == "no_redirect_hops" {
                    ctx.skip(
                        "MISSING_HSTS_ON_REDIRECT_HOP",
                        "no host variant of the start URL answered with a redirect",
                    );
                }
                // Measured with the header on every hop: clean, and silence is
                // the verdict every other clean header check records.
            }
        }
    }
}
