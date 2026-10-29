//! Findings built from retained hyperlink occurrences and observed HTTP outcomes
//! (reference the reference suite, wired in the reference suite).

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};
use spiderhead_parse::py_url::urljoin;

use super::details;
use super::stored::SELECTED_LINKS;
use crate::audit::AuditContext;

const MAX_LOCATIONS: usize = 200;
const HOP_CAP: usize = 20;

struct Edge {
    link_id: i64,
    source_id: i64,
    source: String,
    destination: String,
    anchor: String,
    position: String,
    nofollow: bool,
    target: Option<usize>,
    internal: bool,
}

fn defrag(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

fn blank_anchor(anchor: &str) -> bool {
    anchor
        .trim_matches(['\t', '\n', '\r', '\u{a0}', ' '])
        .is_empty()
}

fn location(e: &Edge) -> Value {
    json!({
        "source_url": e.source,
        "destination_url": e.destination,
        "anchor": e.anchor,
        "link_position": if e.position.is_empty() { Value::Null } else { json!(e.position) },
        "follow": !e.nofollow,
    })
}

/// Sample (first `MAX_LOCATIONS` by link id), total occurrences and distinct sources of `edges`.
fn sample(edges: &[&Edge]) -> (Vec<Value>, usize, usize) {
    let mut sorted: Vec<&&Edge> = edges.iter().collect();
    sorted.sort_by_key(|e| e.link_id);
    let sources: std::collections::BTreeSet<i64> = edges.iter().map(|e| e.source_id).collect();
    (
        sorted
            .iter()
            .take(MAX_LOCATIONS)
            .map(|e| location(e))
            .collect(),
        edges.len(),
        sources.len(),
    )
}

/// `RetainedLinkFindings.findings` over the stored scan: status, empty-anchor and redirect findings.
pub fn check_retained_links(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else { return };
    let start_host: String = con
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|u| spiderhead_parse::urlutil::hostname(&u).ok().flatten())
        .unwrap_or_default();
    let by_url: HashMap<String, usize> = ctx
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.url.clone(), i))
        .collect();
    let sql = format!(
        "{SELECTED_LINKS}SELECT l.link_id, l.source_url_id, src.url, dst.url, l.anchor, l.position, l.nofollow \
         FROM l JOIN urls src ON src.url_id=l.source_url_id JOIN urls dst ON dst.url_id=l.destination_url_id"
    );
    let mut edges: Vec<Edge> = Vec::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        });
        for (link_id, source_id, source, destination, anchor, position, nofollow) in
            rows.into_iter().flatten().flatten()
        {
            let target = by_url.get(defrag(&destination)).copied();
            let internal = spiderhead_parse::urlutil::hostname(&destination)
                .ok()
                .flatten()
                .is_some_and(|h| h == start_host);
            edges.push(Edge {
                link_id,
                source_id,
                source,
                destination,
                anchor,
                position,
                nofollow: nofollow != 0,
                target,
                internal,
            });
        }
    }
    // The skip judges retained hyperlink occurrences, not the CTE-selected
    // subset: a scan whose edges are all filtered still has evidence.
    let no_links = con
        .query_row("SELECT 1 FROM links LIMIT 1", [], |_| Ok(()))
        .is_err();

    // status findings: one per internal target with a 3xx/4xx/5xx answer, in page order of first appearance by target id
    let mut by_target: BTreeMap<usize, Vec<&Edge>> = BTreeMap::new();
    for e in edges.iter().filter(|e| e.internal) {
        if let Some(t) = e.target {
            by_target.entry(t).or_default().push(e);
        }
    }
    let mut targets: Vec<(i64, usize)> = by_target
        .keys()
        .map(|&t| {
            (
                ctx.pages[t]
                    .rec
                    .get("url_id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                t,
            )
        })
        .collect();
    targets.sort();
    for (_, t) in targets {
        let page = ctx.pages[t].clone();
        let Some(status) = page.status_code.filter(|s| (300..600).contains(s)) else {
            continue;
        };
        let check = match status {
            400..=499 => "BROKEN_INTERNAL_LINK",
            500..=599 => "LINK_TO_5XX",
            _ => "INTERNAL_LINK_TO_REDIRECT",
        };
        let (shown, total, sources) = sample(&by_target[&t]);
        let omitted = total - shown.len();
        ctx.add_located(
            check,
            &page.url,
            Some(status),
            sources as i64,
            shown,
            details([
                ("destination_scope", "internal".into()),
                ("link_occurrences", total.into()),
                ("source_pages", sources.into()),
                ("locations_omitted", omitted.into()),
            ]),
        );
        if let Some(issue) = ctx.issues.last_mut() {
            issue.evidence = details([
                ("source", "retained hyperlinks and fetched target".into()),
                ("all_locations", "scan-url-links --direction in".into()),
            ]);
        }
    }

    // empty anchors, one finding per source page
    let mut by_source: BTreeMap<i64, Vec<&Edge>> = BTreeMap::new();
    for e in edges
        .iter()
        .filter(|e| e.internal && blank_anchor(&e.anchor))
    {
        by_source.entry(e.source_id).or_default().push(e);
    }
    for (_, group) in by_source {
        let (shown, total, _) = sample(&group);
        let omitted = total - shown.len();
        ctx.add_located(
            "INTERNAL_LINK_NO_ANCHOR",
            &group[0].source,
            None,
            total as i64,
            shown,
            details([
                (
                    "reason",
                    "internal hyperlink has empty retained anchor text".into(),
                ),
                ("locations_omitted", omitted.into()),
            ]),
        );
    }

    redirect_findings(ctx, &edges, &by_url);
    external_findings(ctx, &con);
    if no_links {
        for check in [
            "BROKEN_INTERNAL_LINK",
            "LINK_TO_5XX",
            "INTERNAL_LINK_TO_REDIRECT",
            "INTERNAL_LINK_NO_ANCHOR",
        ] {
            ctx.skip(check, "native scan retains no hyperlink occurrences");
        }
    }
    ctx.scan = Some(con);
}

#[derive(Clone)]
struct Hop {
    url: String,
    status: Option<i64>,
    location: String,
}

fn redirect_findings(ctx: &mut AuditContext, edges: &[Edge], by_url: &HashMap<String, usize>) {
    let firsts: Vec<usize> = (0..ctx.pages.len())
        .filter(|&i| {
            let p = &ctx.pages[i];
            p.status_code.is_some_and(|s| (300..400).contains(&s))
                && !p.text("redirect_url").is_empty()
        })
        .collect();
    for first in firsts {
        let first_page = ctx.pages[first].clone();
        let mut current = Hop {
            url: first_page.url.clone(),
            status: first_page.status_code,
            location: first_page.text("redirect_url").to_owned(),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut path: Vec<Hop> = Vec::new();
        let (mut redirects, mut looped) = (0usize, false);
        let reason: String;
        loop {
            if seen.contains(&current.url) {
                path.push(current.clone());
                looped = true;
                reason = "retained redirect path revisits a URL".into();
                break;
            }
            seen.push(current.url.clone());
            path.push(current.clone());
            if current.status.is_none_or(|s| !(300..400).contains(&s))
                || current.location.is_empty()
            {
                reason = match current.status {
                    None => "terminal target was not fetched",
                    Some(s) if s >= 400 => "redirect target has an HTTP error",
                    _ => "",
                }
                .into();
                break;
            }
            if redirects >= HOP_CAP {
                reason = "retained redirect hop budget exhausted".into();
                break;
            }
            redirects += 1;
            let target = urljoin(&current.url, &current.location).unwrap_or_default();
            if let Some(&i) = by_url.get(defrag(&target)) {
                let p = &ctx.pages[i];
                current = Hop {
                    url: p.url.clone(),
                    status: p.status_code,
                    location: p.text("redirect_url").to_owned(),
                };
                continue;
            }
            let captured: Vec<Value> =
                serde_json::from_str(first_page.text("redirect_chain_json")).unwrap_or_default();
            let hop = captured.iter().find(|h| h["url"] == json!(target));
            current = Hop {
                url: target,
                status: hop.and_then(|h| h["status_code"].as_i64()),
                location: hop
                    .and_then(|h| {
                        h.get("location")
                            .or_else(|| h.get("redirect_url"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("")
                    .to_owned(),
            };
        }
        if !looped && redirects < 2 {
            continue;
        }
        let url_id = first_page
            .rec
            .get("url_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let inbound: Vec<&Edge> = edges
            .iter()
            .filter(|e| {
                e.internal
                    && e.target.is_some_and(|t| {
                        ctx.pages[t].rec.get("url_id").and_then(Value::as_i64) == Some(url_id)
                    })
            })
            .collect();
        let (shown, total, sources) = sample(&inbound);
        let statuses: Vec<i64> = path
            .iter()
            .filter_map(|h| h.status)
            .filter(|s| (300..400).contains(s))
            .collect();
        let last = path.last().cloned().unwrap_or(Hop {
            url: String::new(),
            status: None,
            location: String::new(),
        });
        let budget = reason == "retained redirect hop budget exhausted";
        let hop_list: Vec<Value> = path
            .iter()
            .map(|h| json!({"url": h.url, "status_code": h.status, "location": h.location}))
            .collect();
        let final_url = if looped || last.status.is_none() || budget {
            Value::Null
        } else {
            json!(last.url)
        };
        let omitted = total - shown.len();
        ctx.add_located(
            if looped {
                "REDIRECT_LOOP"
            } else {
                "REDIRECT_CHAIN"
            },
            &first_page.url,
            first_page.status_code,
            1,
            shown.clone(),
            details([
                ("hops", redirects.into()),
                (
                    "path",
                    Value::Array(path.iter().map(|h| json!(h.url)).collect()),
                ),
                ("hop_list", Value::Array(hop_list)),
                ("final_url", final_url),
                ("terminal_status", json!(last.status)),
                (
                    "mixed_permanent_temporary",
                    json!(
                        statuses.iter().any(|s| matches!(s, 301 | 308))
                            && statuses.iter().any(|s| matches!(s, 302 | 303 | 307))
                    ),
                ),
                (
                    "unresolved",
                    json!(!reason.is_empty() && !looped && (last.status.is_none() || budget)),
                ),
                ("reason", json!(reason)),
                ("inbound_links", Value::Array(shown)),
                ("inbound_links_count", total.into()),
                ("source_pages", sources.into()),
                ("locations_omitted", omitted.into()),
            ]),
        );
    }
}

/// `external_findings`: failed destinations the external-check phase captured,
/// minus safety refusals and skipped budgets. No `external_checks` table means
/// the phase never ran and there is simply nothing to judge.
fn external_findings(ctx: &mut AuditContext, con: &rusqlite::Connection) {
    let Ok(mut stmt) = con.prepare(
        "SELECT payload_json FROM external_checks WHERE outcome='failed' ORDER BY ordinal",
    ) else {
        return;
    };
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect::<Vec<_>>());
    let Ok(rows) = rows else { return };
    for raw in rows {
        let Ok(item) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if matches!(
            item["error_kind"].as_str(),
            Some("blocked_redirect" | "private_network" | "invalid_url")
        ) {
            continue;
        }
        let url = item["url"].as_str().unwrap_or("").to_owned();
        let source_count = item["source_count"].as_i64().unwrap_or(0);
        let sources = item["sources"].as_array();
        let locations: Vec<Value> = sources
            .map(|list| {
                list.iter()
                    .map(|source| json!({"source_url": source}))
                    .collect()
            })
            .unwrap_or_default();
        let sources_omitted = source_count - sources.map_or(0, |list| list.len() as i64);
        let chain = item.get("redirect_chain").cloned().unwrap_or(Value::Null);
        let failure_url = chain
            .as_array()
            .and_then(|hops| hops.last())
            .and_then(|hop| hop["url"].as_str())
            .unwrap_or(&url)
            .to_owned();
        ctx.add_located(
            "EXTERNAL_NO_RESPONSE",
            &url,
            None,
            source_count.max(1),
            locations,
            details([
                ("reason", item["reason"].clone()),
                ("error", item["error"].clone()),
                ("error_kind", item["error_kind"].clone()),
                ("failure_url", json!(failure_url)),
                ("hop_list", chain),
                ("sources_omitted", json!(sources_omitted)),
            ]),
        );
    }
}
