//! Stored link- and form-edge predicates of `_audit_crawl_result`
//! (reference the reference suite: the `has_link_evidence` /
//! `has_form_evidence` / `link_attributes.capture` blocks over `StoredGraph`).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url;
use spiderhead_parse::urlutil::hostname;

use super::details;
use super::stored::SELECTED_LINKS;
use crate::audit::AuditContext;

/// How many blocked targets one finding quotes back (reference `_BLOCKED_TARGET_ITEMS`).
const BLOCKED_TARGET_ITEMS: usize = 5;
/// Bounded destination/source samples of the nofollow predicates (`max_destinations`/`max_sources`).
const MAX_LISTED: usize = 20;
/// Rel tokens that prevent reverse tabnabbing on a `target="_blank"` link.
const SAFE_BLANK_REL: [&str; 2] = ["noopener", "noreferrer"];
/// Rel tokens that describe a link leaving the site.
const OUTBOUND_HINT_REL: [&str; 2] = ["sponsored", "ugc"];
/// Rel tokens reviewed by `mixed_rel_links`/`rel_summary`.
const QUALIFYING_REL: [&str; 3] = ["nofollow", "sponsored", "ugc"];

/// One selected link edge with the attribute columns the predicates read.
struct Edge {
    source: String,
    destination: String,
    nofollow: bool,
    rel: Vec<String>,
    target: String,
    raw_href: String,
}

/// `StoredGraph.iter_links`: selected edges in stored page/ordinal order.
fn iter_links(con: &Connection) -> Vec<Edge> {
    let sql = format!(
        "{SELECTED_LINKS}SELECT l.*, src.url AS source, dst.url AS destination \
         FROM l JOIN pages AS sp ON sp.url_id=l.source_url_id \
         JOIN urls AS src ON src.url_id=l.source_url_id \
         JOIN urls AS dst ON dst.url_id=l.destination_url_id \
         ORDER BY sp.page_ordinal, l.ordinal, l.link_id"
    );
    let mut edges = Vec::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        let names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        if let Ok(mut rows) = stmt.query(params![]) {
            while let Ok(Some(row)) = rows.next() {
                let get = |name: &str| -> usize {
                    names.iter().position(|n| n == name).unwrap_or(usize::MAX)
                };
                let rel: Vec<String> = row
                    .get::<_, String>(get("rel_json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                edges.push(Edge {
                    source: row.get::<_, String>(get("source")).unwrap_or_default(),
                    destination: row.get::<_, String>(get("destination")).unwrap_or_default(),
                    nofollow: row.get::<_, i64>(get("nofollow")).unwrap_or(0) != 0,
                    rel,
                    target: row.get::<_, String>(get("target")).unwrap_or_default(),
                    raw_href: row.get::<_, String>(get("raw_href")).unwrap_or_default(),
                });
            }
        }
    }
    edges
}

/// Lower-cased rel token set of one edge.
fn rel_tokens(edge: &Edge) -> BTreeSet<String> {
    edge.rel.iter().map(|t| t.to_lowercase()).collect()
}

/// `link_findings._origin`: normalized (scheme, hostname, effective port).
fn origin(url: &str) -> (String, String, Option<i64>) {
    let parts = py_url::urlsplit(url).unwrap_or_default();
    let scheme = parts.scheme.to_lowercase();
    let host = hostname(url).ok().flatten().unwrap_or_default();
    let hostport = parts.netloc.rsplit('@').next().unwrap_or("");
    let explicit = if hostport.starts_with('[') {
        hostport
            .rsplit(']')
            .next()
            .and_then(|tail| tail.strip_prefix(':'))
            .and_then(|p| p.parse::<i64>().ok())
    } else {
        hostport
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<i64>().ok())
    };
    let port = explicit.or(match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    });
    (scheme, host, port)
}

/// Hostname of `url`, lower-cased; empty when absent (reference `urlsplit(...).hostname or ""`).
fn host_of(url: &str) -> String {
    hostname(url).ok().flatten().unwrap_or_default()
}

/// Scheme of `url`, lower-cased (reference `urlsplit(url).scheme.lower()`).
fn scheme_of(url: &str) -> String {
    py_url::urlsplit(url)
        .map(|p| p.scheme.to_lowercase())
        .unwrap_or_default()
}

/// `list_mode` context marker (`crawl/list_scan.is_list_scan`).
pub(crate) fn stored_list(con: &Connection) -> bool {
    con.query_row(
        "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Robots-disallowed URL identities recorded on the scan (`result.robots_blocked`
/// on the stored path -- reasons are never retained, so targets stay bare URLs).
fn blocked_target_set(con: &Connection) -> BTreeSet<String> {
    let mut blocked = BTreeSet::new();
    if let Ok(mut stmt) = con.prepare(
        "SELECT u.url FROM context_items c JOIN urls u \
         ON u.url_id=json_extract(c.payload_json,'$.url_id') \
         WHERE c.kind='robots_blocked_url' ORDER BY c.item_key",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            blocked = rows.flatten().collect();
        }
    }
    blocked
}

/// `robots_blocked_outlinks`: one finding per source page pointing at a
/// robots-disallowed target; `blocked_targets` keeps at most five sorted URLs.
fn robots_blocked_outlinks(edges: &[Edge], blocked: &BTreeSet<String>) -> Vec<Map<String, Value>> {
    // First-seen source order mirrors the dict population of the reference loop.
    let mut by_source: Vec<(String, Vec<String>)> = Vec::new();
    for edge in edges {
        if !blocked.contains(&edge.destination) {
            continue;
        }
        match by_source.iter_mut().find(|(s, _)| *s == edge.source) {
            Some((_, targets)) => targets.push(edge.destination.clone()),
            None => by_source.push((edge.source.clone(), vec![edge.destination.clone()])),
        }
    }
    by_source
        .into_iter()
        .map(|(source, targets)| {
            let shown: Vec<Value> = targets
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(BLOCKED_TARGET_ITEMS)
                .map(Value::from)
                .collect();
            details([
                ("target_url", json!(source)),
                ("count", json!(targets.len() as i64)),
                ("blocked_targets", json!(shown)),
            ])
        })
        .collect()
}

/// `follow_and_nofollow_inlinks`: internal destinations linked both with and
/// without `nofollow` (sorted destination list; the stored path emits the
/// detail variant, so this stays test-only).
#[cfg(test)]
fn follow_and_nofollow_inlinks(edges: &[Edge], host: &str) -> Vec<String> {
    let host = host.to_lowercase();
    let mut flags: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for edge in edges {
        if host_of(&edge.destination) != host {
            continue;
        }
        let item = flags.entry(edge.destination.clone()).or_default();
        if edge.nofollow {
            item.1 = true;
        } else {
            item.0 = true;
        }
    }
    flags
        .into_iter()
        .filter(|(_, (follow, nofollow))| *follow && *nofollow)
        .map(|(destination, _)| destination)
        .collect()
}

/// `follow_and_nofollow_inlink_details`: mixed internal rel states per destination.
fn follow_and_nofollow_inlink_details(
    edges: &[Edge],
    host: &str,
    max_sources: usize,
) -> Vec<Map<String, Value>> {
    struct Group {
        sources: BTreeSet<String>,
        follow: i64,
        nofollow: i64,
    }
    let host = host.to_lowercase();
    let mut grouped: BTreeMap<String, Group> = BTreeMap::new();
    for edge in edges {
        if host_of(&edge.destination) != host {
            continue;
        }
        let item = grouped.entry(edge.destination.clone()).or_insert(Group {
            sources: BTreeSet::new(),
            follow: 0,
            nofollow: 0,
        });
        item.sources.insert(edge.source.clone());
        if edge.nofollow {
            item.nofollow += 1;
        } else {
            item.follow += 1;
        }
    }
    grouped
        .into_iter()
        .filter(|(_, item)| item.follow > 0 && item.nofollow > 0)
        .map(|(destination, item)| {
            details([
                ("target_url", json!(destination)),
                (
                    "sources",
                    json!(item.sources.iter().take(max_sources).collect::<Vec<_>>()),
                ),
                ("follow_occurrences", json!(item.follow)),
                ("nofollow_occurrences", json!(item.nofollow)),
            ])
        })
        .collect()
}

/// `internal_nofollow_outlinks`/`external_nofollow_outlinks`: source pages with a
/// bounded destination sample; `external` keeps only off-host destinations.
fn nofollow_outlinks_items(
    edges: &[Edge],
    host: &str,
    external: bool,
    max_destinations: usize,
) -> Vec<Map<String, Value>> {
    struct Group {
        destinations: BTreeSet<String>,
        occurrences: i64,
    }
    let host = host.to_lowercase();
    let mut grouped: BTreeMap<String, Group> = BTreeMap::new();
    for edge in edges {
        if !edge.nofollow {
            continue;
        }
        let dest_host = host_of(&edge.destination);
        let matches = if external {
            !dest_host.is_empty() && dest_host != host
        } else {
            dest_host == host
        };
        if !matches {
            continue;
        }
        let item = grouped.entry(edge.source.clone()).or_insert(Group {
            destinations: BTreeSet::new(),
            occurrences: 0,
        });
        item.destinations.insert(edge.destination.clone());
        item.occurrences += 1;
    }
    grouped
        .into_iter()
        .map(|(source, item)| {
            details([
                ("target_url", json!(source)),
                ("nofollow_occurrences", json!(item.occurrences)),
                (
                    "destinations",
                    json!(item
                        .destinations
                        .iter()
                        .take(max_destinations)
                        .collect::<Vec<_>>()),
                ),
            ])
        })
        .collect()
}

/// `http_links_on_https_pages`: internal `http://` anchors on HTTPS pages whose
/// target did not already converge to HTTPS.
fn http_links_on_https_pages(
    edges: &[Edge],
    host: &str,
    upgraded: &BTreeSet<String>,
) -> Vec<Map<String, Value>> {
    let host = host.to_lowercase();
    let mut out = Vec::new();
    for edge in edges {
        if scheme_of(&edge.source) != "https"
            || host_of(&edge.source) != host
            || host_of(&edge.destination) != host
            || !edge.raw_href.to_lowercase().starts_with("http://")
            || upgraded.contains(&edge.destination)
        {
            continue;
        }
        out.push(details([
            ("target_url", json!(edge.source)),
            ("destination", json!(edge.destination)),
            ("raw_href", json!(edge.raw_href)),
        ]));
    }
    out
}

/// `unsafe_cross_origin_links`: cross-origin `target="_blank"` without noopener/noreferrer.
fn unsafe_cross_origin_links(edges: &[Edge]) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    for edge in edges {
        if edge.target.to_lowercase() != "_blank" {
            continue;
        }
        if origin(&edge.source) == origin(&edge.destination) {
            continue;
        }
        let tokens = rel_tokens(edge);
        if tokens.iter().any(|t| SAFE_BLANK_REL.contains(&t.as_str())) {
            continue;
        }
        out.push(details([
            ("target_url", json!(edge.source)),
            ("destination", json!(edge.destination)),
        ]));
    }
    out
}

/// `protocol_relative_links`: edges written as `//host/path`.
fn protocol_relative_links(edges: &[Edge]) -> Vec<Map<String, Value>> {
    edges
        .iter()
        .filter(|edge| edge.raw_href.starts_with("//"))
        .map(|edge| {
            details([
                ("target_url", json!(edge.source)),
                ("destination", json!(edge.destination)),
                ("raw_href", json!(edge.raw_href)),
            ])
        })
        .collect()
}

/// `internal_sponsored_ugc_links`: same-host edges carrying `sponsored`/`ugc`.
fn internal_sponsored_ugc_links(edges: &[Edge], host: &str) -> Vec<Map<String, Value>> {
    let host = host.to_lowercase();
    let mut out = Vec::new();
    for edge in edges {
        if host_of(&edge.destination) != host {
            continue;
        }
        let tokens = rel_tokens(edge);
        if !tokens
            .iter()
            .any(|t| OUTBOUND_HINT_REL.contains(&t.as_str()))
        {
            continue;
        }
        out.push(details([
            ("target_url", json!(edge.source)),
            ("destination", json!(edge.destination)),
            ("rel", json!(tokens.into_iter().collect::<Vec<_>>())),
        ]));
    }
    out
}

/// `mixed_rel_links`: edges combining two or more qualifying rel tokens.
fn mixed_rel_links(edges: &[Edge]) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    for edge in edges {
        let tokens = rel_tokens(edge);
        let qualifying: Vec<&String> = tokens
            .iter()
            .filter(|t| QUALIFYING_REL.contains(&t.as_str()))
            .collect();
        if qualifying.len() <= 1 {
            continue;
        }
        out.push(details([
            ("target_url", json!(edge.source)),
            ("destination", json!(edge.destination)),
            ("rel", json!(tokens.iter().collect::<Vec<_>>())),
            (
                "qualifying_rel",
                json!(qualifying.into_iter().collect::<Vec<_>>()),
            ),
            ("combination_valid", json!(true)),
        ]));
    }
    out
}

/// `form_url_insecure`: forms whose action submits over plain HTTP.
fn form_url_insecure(forms: &[FormEdge]) -> Vec<Map<String, Value>> {
    forms
        .iter()
        .filter(|f| f.action.to_lowercase().starts_with("http://"))
        .map(|f| {
            details([
                ("target_url", json!(f.page)),
                ("action", json!(f.action)),
                ("method", json!(f.method)),
            ])
        })
        .collect()
}

/// `forms_on_http_pages_with_password`: password forms served from plain-HTTP pages.
fn forms_on_http_pages_with_password(forms: &[FormEdge]) -> Vec<Map<String, Value>> {
    forms
        .iter()
        .filter(|f| f.has_password && f.page.to_lowercase().starts_with("http://"))
        .map(|f| details([("target_url", json!(f.page)), ("action", json!(f.action))]))
        .collect()
}

/// Emit one finding per predicate item (`ctx.add(check, target_url=item["target_url"], details=item)`).
fn emit(ctx: &mut AuditContext, check: &str, items: Vec<Map<String, Value>>) {
    for item in items {
        let target = item
            .get("target_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        ctx.add(check, target.as_deref(), item);
    }
}

/// `rel_summary`: independent per-token counts; mixed edges contribute to each class.
fn rel_summary(edges: &[Edge]) -> Map<String, Value> {
    let mut total = 0_i64;
    let mut nofollow = 0_i64;
    let mut ugc = 0_i64;
    let mut sponsored = 0_i64;
    let mut mixed = 0_i64;
    let mut unqualified = 0_i64;
    for edge in edges {
        let tokens = rel_tokens(edge);
        let qualifying: BTreeSet<&String> = tokens
            .iter()
            .filter(|t| QUALIFYING_REL.contains(&t.as_str()))
            .collect();
        total += 1;
        for token in &qualifying {
            match token.as_str() {
                "nofollow" => nofollow += 1,
                "ugc" => ugc += 1,
                "sponsored" => sponsored += 1,
                _ => {}
            }
        }
        mixed += i64::from(qualifying.len() > 1);
        unqualified += i64::from(qualifying.is_empty());
    }
    let mut by_rel = Map::new();
    by_rel.insert("total".into(), json!(total));
    by_rel.insert("nofollow".into(), json!(nofollow));
    by_rel.insert("ugc".into(), json!(ugc));
    by_rel.insert("sponsored".into(), json!(sponsored));
    by_rel.insert("mixed".into(), json!(mixed));
    by_rel.insert("unqualified".into(), json!(unqualified));
    let mut out = Map::new();
    out.insert("measured".into(), json!(true));
    out.insert("by_rel".into(), Value::Object(by_rel));
    out.insert("counts_overlap".into(), json!(true));
    out
}

/// `StoredGraph.sponsored_disclosures`: replay each donor document once per
/// sponsored off-host edge, then classify the disclosure marker.
fn sponsored_disclosures(con: &Connection, host: &str) -> Map<String, Value> {
    let sql = format!(
        "{SELECTED_LINKS}SELECT l.*,src.url AS source,dst.url AS destination FROM l \
         JOIN urls src ON src.url_id=l.source_url_id \
         JOIN urls dst ON dst.url_id=l.destination_url_id \
         ORDER BY l.source_url_id,l.source_document_id,l.link_id"
    );
    let mut findings: Vec<Map<String, Value>> = Vec::new();
    let mut candidates = 0_i64;
    let mut measured = 0_i64;
    let mut disclosed = 0_i64;
    let mut unavailable = 0_i64;
    let mut reasons: Vec<(String, i64)> = Vec::new();
    let mut current: Option<i64> = None;
    let mut contexts: Map<String, Value> = Map::new();
    let host = host.to_lowercase();
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(mut rows) = stmt.query(params![]) {
            while let Ok(Some(row)) = rows.next() {
                let rel_json: String = row.get::<_, String>("rel_json").unwrap_or_default();
                let tokens: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&rel_json)
                    .unwrap_or_default()
                    .iter()
                    .map(|t| t.to_lowercase())
                    .collect();
                let destination: String = row.get("destination").unwrap_or_default();
                if !tokens.contains("sponsored") || host_of(&destination) == host {
                    continue;
                }
                candidates += 1;
                let source_document_id: Option<i64> = row
                    .get::<_, Option<i64>>("source_document_id")
                    .ok()
                    .flatten();
                let link_id: i64 = row.get("link_id").unwrap_or(0);
                let mut reason = "source document was not retained".to_string();
                if let Some(document_id) = source_document_id {
                    if current != Some(document_id) {
                        contexts = match spiderhead_store::link_context::links(con, document_id)
                            .and_then(|links| {
                                spiderhead_store::link_context::derive(
                                    con,
                                    document_id,
                                    &links,
                                    8 * 1024 * 1024,
                                )
                            }) {
                            // A corrupt binding raises in reference too; on a valid
                            // scan this path is unreachable.
                            Ok((items, _coverage)) => items
                                .into_iter()
                                .filter_map(|item| {
                                    item.get("link_id")
                                        .and_then(Value::as_i64)
                                        .map(|id| (id.to_string(), Value::Object(item)))
                                })
                                .collect(),
                            Err(_) => Map::new(),
                        };
                        current = Some(document_id);
                    }
                    let item = contexts.get(&link_id.to_string());
                    // the reference `and disclosure` is truthiness: an empty replayed
                    // mapping reads as absent, not as a measured non-disclosure.
                    let disclosure = item
                        .and_then(|i| i.get("disclosure"))
                        .and_then(Value::as_object)
                        .filter(|m| !m.is_empty())
                        .cloned();
                    let measured_item = item.and_then(|i| i.get("state")).and_then(Value::as_str)
                        == Some("measured");
                    if measured_item && disclosure.is_some() {
                        let disclosure = disclosure.unwrap_or_default();
                        if disclosure
                            .get("detected")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            disclosed += 1;
                            measured += 1;
                        } else if disclosure.get("state").and_then(Value::as_str)
                            == Some("measured")
                        {
                            measured += 1;
                            let source: String = row.get("source").unwrap_or_default();
                            let mut finding = Map::new();
                            finding.insert("target_url".into(), json!(source));
                            finding.insert("destination".into(), json!(destination));
                            finding.insert("link_id".into(), json!(link_id));
                            finding.insert("source_document_id".into(), json!(document_id));
                            finding.insert("rel".into(), json!(tokens.iter().collect::<Vec<_>>()));
                            finding.insert("disclosure".into(), Value::Object(disclosure));
                            findings.push(finding);
                        } else {
                            reason = "local disclosure context exceeded the text budget".into();
                            unavailable += 1;
                            bump(&mut reasons, &reason);
                        }
                    } else {
                        reason = item
                            .and_then(|i| i.get("reason"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("sponsored context was not replayed")
                            .to_string();
                        unavailable += 1;
                        bump(&mut reasons, &reason);
                    }
                } else {
                    unavailable += 1;
                    bump(&mut reasons, &reason);
                }
            }
        }
    }
    let mut coverage = Map::new();
    coverage.insert("candidates".into(), json!(candidates));
    coverage.insert("measured".into(), json!(measured));
    coverage.insert("disclosed".into(), json!(disclosed));
    coverage.insert("unavailable".into(), json!(unavailable));
    let mut out = Map::new();
    out.insert("findings".into(), json!(findings));
    out.insert("coverage".into(), Value::Object(coverage));
    out.insert(
        "state".into(),
        json!(if unavailable > 0 {
            "partial"
        } else {
            "complete"
        }),
    );
    out.insert(
        "unavailable_reasons".into(),
        Value::Object(
            reasons
                .into_iter()
                .map(|(k, v)| (k, json!(v)))
                .collect::<Map<_, _>>(),
        ),
    );
    out.insert("heuristic".into(), json!(true));
    out
}

/// First-seen insertion order reason counter (reference dict `reasons[reason] += 1`).
fn bump(reasons: &mut Vec<(String, i64)>, reason: &str) {
    match reasons.iter_mut().find(|(r, _)| r == reason) {
        Some((_, n)) => *n += 1,
        None => reasons.push((reason.to_string(), 1)),
    }
}

/// One stored form edge (`StoredGraph.iter_forms`).
struct FormEdge {
    page: String,
    method: String,
    action: String,
    has_password: bool,
}

fn iter_forms(con: &Connection) -> Vec<FormEdge> {
    let mut forms = Vec::new();
    if let Ok(mut stmt) = con.prepare(
        "SELECT page_url.url AS page, f.method, f.action, f.has_password \
         FROM forms AS f JOIN pages AS parent ON parent.url_id=f.page_url_id \
         JOIN urls AS page_url ON page_url.url_id=f.page_url_id \
         ORDER BY parent.page_ordinal, f.evidence_representation, f.ordinal, f.form_id",
    ) {
        if let Ok(mut rows) = stmt.query(params![]) {
            while let Ok(Some(row)) = rows.next() {
                forms.push(FormEdge {
                    page: row.get::<_, String>("page").unwrap_or_default(),
                    method: row.get::<_, String>("method").unwrap_or_default(),
                    action: row.get::<_, String>("action").unwrap_or_default(),
                    has_password: row.get::<_, i64>("has_password").unwrap_or(0) != 0,
                });
            }
        }
    }
    forms
}

/// The whole link/form predicate block of `_audit_crawl_result` for a stored
/// scan. Returns the `summary.link_rel` value.
pub fn run_link_predicates(ctx: &mut AuditContext) -> Map<String, Value> {
    let mut link_rel = Map::new();
    link_rel.insert("measured".into(), json!(false));
    link_rel.insert(
        "reason".into(),
        json!("full link attributes were not retained"),
    );

    let stored_list = ctx.scan.as_ref().is_some_and(stored_list);
    let has_link_evidence = !stored_list && ctx.scan.is_some();
    let has_form_evidence = has_link_evidence;
    let capture = ctx
        .config
        .get("link_attributes")
        .and_then(|m| m.get("capture"))
        .and_then(Value::as_bool)
        == Some(true);

    let edges = if has_link_evidence {
        let con = ctx.scan.take().expect("checked above");
        let edges = iter_links(&con);
        ctx.scan = Some(con);
        edges
    } else {
        Vec::new()
    };

    if has_link_evidence {
        // OUTLINK_TO_LOCALHOST iterates the same selected edges in the same
        // order; the stored module owns that emission.
        super::stored::check_localhost_outlinks(ctx);
        let robots_policy = ctx
            .config
            .get("robots")
            .and_then(|m| m.get("policy"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if robots_policy == "ignore" {
            ctx.skip(
                "ROBOTS_BLOCKED_OUTLINK",
                "robots.policy is ignore: no URL was measured against robots.txt",
            );
        } else {
            let con = ctx.scan.take().expect("checked above");
            let blocked = blocked_target_set(&con);
            ctx.scan = Some(con);
            emit(
                ctx,
                "ROBOTS_BLOCKED_OUTLINK",
                robots_blocked_outlinks(&edges, &blocked),
            );
        }
        let crawl_host = ctx.start_url.as_deref().map(host_of).unwrap_or_default();
        if !crawl_host.is_empty() {
            emit(
                ctx,
                "FOLLOW_AND_NOFOLLOW_INLINKS",
                follow_and_nofollow_inlink_details(&edges, &crawl_host, MAX_LISTED),
            );
            emit(
                ctx,
                "INTERNAL_NOFOLLOW_OUTLINKS",
                nofollow_outlinks_items(&edges, &crawl_host, false, MAX_LISTED),
            );
            emit(
                ctx,
                "EXTERNAL_NOFOLLOW",
                nofollow_outlinks_items(&edges, &crawl_host, true, MAX_LISTED),
            );
            if capture {
                let mut upgraded = BTreeSet::new();
                for page in &ctx.pages {
                    let status = page.status_code.unwrap_or(0);
                    let redirect = page
                        .rec
                        .get("redirect_url")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if (300..=399).contains(&status)
                        && redirect.to_lowercase().starts_with("https://")
                    {
                        upgraded.insert(page.url.clone());
                    }
                }
                emit(
                    ctx,
                    "HTTP_LINK_ON_HTTPS",
                    http_links_on_https_pages(&edges, &crawl_host, &upgraded),
                );
            } else {
                ctx.skip(
                    "HTTP_LINK_ON_HTTPS",
                    "link_attributes.capture is false; original href schemes were not retained",
                );
            }
        } else {
            let no_host = "no crawl start URL is available to identify one site's internal links";
            ctx.skip("FOLLOW_AND_NOFOLLOW_INLINKS", no_host);
            ctx.skip("INTERNAL_NOFOLLOW_OUTLINKS", no_host);
            ctx.skip("EXTERNAL_NOFOLLOW", no_host);
        }
    } else {
        let reason = "crawl-list input retains no link-edge evidence";
        ctx.skip("OUTLINK_TO_LOCALHOST", reason);
        ctx.skip("ROBOTS_BLOCKED_OUTLINK", reason);
        ctx.skip("FOLLOW_AND_NOFOLLOW_INLINKS", reason);
        ctx.skip("INTERNAL_NOFOLLOW_OUTLINKS", reason);
        ctx.skip("EXTERNAL_NOFOLLOW", reason);
        ctx.skip("HTTP_LINK_ON_HTTPS", reason);
    }

    if has_form_evidence {
        let con = ctx.scan.take().expect("checked above");
        let forms = iter_forms(&con);
        ctx.scan = Some(con);
        emit(ctx, "FORM_URL_INSECURE", form_url_insecure(&forms));
        emit(
            ctx,
            "FORM_ON_HTTP_URL",
            forms_on_http_pages_with_password(&forms),
        );
    } else {
        let reason = "crawl-list input retains no form evidence";
        ctx.skip("FORM_URL_INSECURE", reason);
        ctx.skip("FORM_ON_HTTP_URL", reason);
    }

    if ctx.start_url.is_some() && has_link_evidence && capture {
        emit(
            ctx,
            "UNSAFE_CROSS_ORIGIN_LINK",
            unsafe_cross_origin_links(&edges),
        );
        emit(
            ctx,
            "PROTOCOL_RELATIVE_LINK",
            protocol_relative_links(&edges),
        );
        let site_host = ctx.start_url.as_deref().map(host_of).unwrap_or_default();
        emit(
            ctx,
            "INTERNAL_LINK_SPONSORED_UGC",
            internal_sponsored_ugc_links(&edges, &site_host),
        );
        emit(ctx, "MIXED_LINK_REL", mixed_rel_links(&edges));
        link_rel = rel_summary(&edges);
        let mut html_total = 0_i64;
        let mut html_unavailable = 0_i64;
        for &i in &ctx.html_pages() {
            html_total += 1;
            html_unavailable += i64::from(ctx.pages[i].body_unavailable());
        }
        let mut coverage = Map::new();
        coverage.insert("html_pages".into(), json!(html_total));
        coverage.insert("html_pages_unavailable".into(), json!(html_unavailable));
        coverage.insert(
            "scope".into(),
            json!("captured link occurrences; unparsed pages are excluded"),
        );
        link_rel.insert("coverage".into(), Value::Object(coverage));
        link_rel.insert(
            "state".into(),
            json!(if html_unavailable > 0 {
                "partial"
            } else {
                "complete"
            }),
        );
        if html_unavailable > 0 {
            link_rel.insert(
                "reason".into(),
                json!(format!(
                    "{html_unavailable} HTML page(s) were not parsed; their rel and disclosure evidence was not measured"
                )),
            );
        }
        let by_rel_total = link_rel
            .get("by_rel")
            .and_then(|b| b.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if html_total == html_unavailable && by_rel_total == 0 {
            let reason = "no parsed HTML page or captured link occurrence provides rel evidence";
            link_rel.insert("measured".into(), json!(false));
            link_rel.insert("state".into(), json!("unavailable"));
            link_rel.insert("reason".into(), json!(reason));
            if let Some(Value::Object(by_rel)) = link_rel.get_mut("by_rel") {
                for key in by_rel.keys().cloned().collect::<Vec<_>>() {
                    by_rel.insert(key, Value::Null);
                }
            }
            ctx.skip("MIXED_LINK_REL", reason);
            ctx.skip("OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE", reason);
        }
        let con = ctx.scan.take().expect("checked above");
        let disclosure = sponsored_disclosures(&con, &site_host);
        ctx.scan = Some(con);
        link_rel.insert(
            "disclosure".into(),
            Value::Object(
                disclosure
                    .iter()
                    .filter(|(k, _)| k.as_str() != "findings")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Map<_, _>>(),
            ),
        );
        if let Some(Value::Array(items)) = disclosure.get("findings").cloned() {
            for item in items {
                let target = item
                    .get("target_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let details_map = match item {
                    Value::Object(m) => m,
                    _ => Map::new(),
                };
                ctx.add(
                    "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
                    target.as_deref(),
                    details_map,
                );
            }
        }
        let unavailable = disclosure
            .get("coverage")
            .and_then(|c| c.get("unavailable"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if unavailable > 0 {
            ctx.skip(
                "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
                "some sponsored-link contexts are unavailable; see summary.link_rel.disclosure",
            );
        }
    } else {
        let reason = if !capture {
            "link_attributes.capture is false; full rel tokens were not retained"
        } else {
            "link evidence or crawl site host is unavailable"
        };
        link_rel.insert("reason".into(), json!(reason));
        ctx.skip("MIXED_LINK_REL", reason);
        ctx.skip("OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE", reason);
    }
    link_rel
}

#[cfg(test)]
mod tests {
    //! Ported pure-predicate tests.
    //!
    //! the reference suite builds `LinkEdge`/`FormEdge` rows and calls
    //! the `link_findings` functions directly; the reference suite
    //! does the same for the blocked-target filter; the reference suite
    //! contributes the mixed-rel summary table and the disclosure-label probes
    //! (which reference runs through `checks.link_context.extract_occurrences`).

    use serde_json::{json, Value};

    use super::*;
    use crate::audit::rules::stored::outlinks_to_localhost;

    const ROOT: &str = "https://example.com/";
    const OUT: &str = "https://partner.example.test/offer";

    fn edge(source: &str, destination: &str) -> Edge {
        Edge {
            source: source.to_string(),
            destination: destination.to_string(),
            nofollow: false,
            rel: Vec::new(),
            target: String::new(),
            raw_href: String::new(),
        }
    }

    fn edge_with(
        source: &str,
        destination: &str,
        nofollow: bool,
        rel: &[&str],
        target: &str,
        raw_href: &str,
    ) -> Edge {
        Edge {
            nofollow,
            rel: rel.iter().map(|t| t.to_string()).collect(),
            target: target.to_string(),
            raw_href: raw_href.to_string(),
            ..edge(source, destination)
        }
    }

    fn form(page: &str, method: &str, action: &str, has_password: bool) -> FormEdge {
        FormEdge {
            page: page.to_string(),
            method: method.to_string(),
            action: action.to_string(),
            has_password,
        }
    }

    fn pair(edges: &[(&str, &str)]) -> Vec<(String, String)> {
        edges
            .iter()
            .map(|(s, d)| (s.to_string(), d.to_string()))
            .collect()
    }

    #[test]
    fn localhost_hostname_is_flagged() {
        let edges = pair(&[(ROOT, "http://localhost:8000/debug")]);
        assert_eq!(
            outlinks_to_localhost(&edges),
            vec![details([
                ("target_url", json!(ROOT)),
                ("destination", json!("http://localhost:8000/debug")),
            ])]
        );
    }

    #[test]
    fn loopback_ip_literal_is_flagged() {
        let edges = pair(&[(ROOT, "http://127.0.0.1/admin")]);
        assert_eq!(outlinks_to_localhost(&edges).len(), 1);
    }

    #[test]
    fn ipv6_loopback_is_flagged() {
        let edges = pair(&[(ROOT, "http://[::1]/x")]);
        assert_eq!(outlinks_to_localhost(&edges).len(), 1);
    }

    #[test]
    fn ordinary_external_host_is_not_flagged() {
        let edges = pair(&[(ROOT, "https://other.example/x")]);
        assert!(outlinks_to_localhost(&edges).is_empty());
    }

    #[test]
    fn dotted_localhost_subdomain_is_flagged() {
        let edges = pair(&[(ROOT, "http://api.localhost/x")]);
        assert_eq!(outlinks_to_localhost(&edges).len(), 1);
    }

    #[test]
    fn http_internal_anchor_on_https_page_requires_raw_href_and_no_safe_redirect() {
        let edges = vec![
            edge_with(
                "https://example.com/source",
                "http://example.com/target",
                false,
                &[],
                "",
                "http://example.com/target",
            ),
            edge_with(
                "https://example.com/source",
                "https://example.com/relative",
                false,
                &[],
                "",
                "/relative",
            ),
        ];
        assert_eq!(
            http_links_on_https_pages(&edges, "example.com", &BTreeSet::new()),
            vec![details([
                ("target_url", json!("https://example.com/source")),
                ("destination", json!("http://example.com/target")),
                ("raw_href", json!("http://example.com/target")),
            ])]
        );
        let upgraded: BTreeSet<String> = ["http://example.com/target".to_string()].into();
        assert!(http_links_on_https_pages(&edges, "example.com", &upgraded).is_empty());
    }

    #[test]
    fn http_link_check_has_positive_and_negative_observed_edge_evidence() {
        let risky = http_links_on_https_pages(
            &[edge_with(
                "https://example.com/source",
                "http://example.com/t",
                false,
                &[],
                "",
                "http://example.com/t",
            )],
            "example.com",
            &BTreeSet::new(),
        );
        assert_eq!(risky.len(), 1);
        assert!(http_links_on_https_pages(&[], "example.com", &BTreeSet::new()).is_empty());
    }

    #[test]
    fn blank_target_without_noopener_or_noreferrer_is_unsafe() {
        let edges = vec![edge_with(
            ROOT,
            "https://other.example/",
            false,
            &[],
            "_blank",
            "",
        )];
        assert_eq!(unsafe_cross_origin_links(&edges).len(), 1);
    }

    #[test]
    fn blank_target_with_noopener_is_safe() {
        let edges = vec![edge_with(
            ROOT,
            "https://other.example/",
            false,
            &["noopener"],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn blank_target_with_noreferrer_is_also_safe() {
        let edges = vec![edge_with(
            ROOT,
            "https://other.example/",
            false,
            &["noreferrer"],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn ordinary_target_is_never_flagged() {
        let edges = vec![edge_with(
            ROOT,
            "https://other.example/",
            false,
            &[],
            "_self",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn edge_with_no_captured_attributes_is_not_a_false_positive() {
        let edges = vec![edge(ROOT, "https://other.example/")];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn same_origin_blank_target_without_rel_is_not_flagged() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://example.test/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn different_host_blank_target_without_rel_remains_flagged() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://other.example/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert_eq!(unsafe_cross_origin_links(&edges).len(), 1);
    }

    #[test]
    fn different_scheme_same_host_is_cross_origin() {
        let edges = vec![edge_with(
            "https://example.test/",
            "http://example.test/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert_eq!(unsafe_cross_origin_links(&edges).len(), 1);
    }

    #[test]
    fn different_explicit_port_same_host_is_cross_origin() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://example.test:8443/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert_eq!(unsafe_cross_origin_links(&edges).len(), 1);
    }

    #[test]
    fn explicit_default_port_is_same_origin_as_implicit() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://example.test:443/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn explicit_default_http_port_is_same_origin_as_implicit() {
        let edges = vec![edge_with(
            "http://example.test/",
            "http://example.test:80/account",
            false,
            &[],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn noopener_suppresses_cross_origin_finding() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://other.example/account",
            false,
            &["noopener"],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn noreferrer_suppresses_cross_origin_finding() {
        let edges = vec![edge_with(
            "https://example.test/",
            "https://other.example/account",
            false,
            &["noreferrer"],
            "_blank",
            "",
        )];
        assert!(unsafe_cross_origin_links(&edges).is_empty());
    }

    #[test]
    fn protocol_relative_href_is_flagged() {
        let edges = vec![edge_with(
            ROOT,
            "https://cdn.example/x.js",
            false,
            &[],
            "",
            "//cdn.example/x.js",
        )];
        assert_eq!(
            protocol_relative_links(&edges),
            vec![details([
                ("target_url", json!(ROOT)),
                ("destination", json!("https://cdn.example/x.js")),
                ("raw_href", json!("//cdn.example/x.js")),
            ])]
        );
    }

    #[test]
    fn ordinary_relative_href_is_not_flagged() {
        let edges = vec![edge_with(
            ROOT,
            "https://example.com/a",
            false,
            &[],
            "",
            "/a",
        )];
        assert!(protocol_relative_links(&edges).is_empty());
    }

    #[test]
    fn absolute_href_is_not_flagged() {
        let edges = vec![edge_with(
            ROOT,
            "https://other.example/x",
            false,
            &[],
            "",
            "https://other.example/x",
        )];
        assert!(protocol_relative_links(&edges).is_empty());
    }

    #[test]
    fn unmeasured_raw_href_is_not_a_false_positive() {
        let edges = vec![edge(ROOT, "https://other.example/x")];
        assert!(protocol_relative_links(&edges).is_empty());
    }

    #[test]
    fn internal_sponsored_or_ugc_link_is_flagged() {
        let edges = vec![
            edge_with(
                ROOT,
                "https://example.com/a",
                false,
                &["ugc", "nofollow"],
                "",
                "",
            ),
            edge_with(ROOT, "https://example.com/b", false, &["Sponsored"], "", ""),
        ];
        let found = internal_sponsored_ugc_links(&edges, "example.com");
        let destinations: Vec<&Value> = found
            .iter()
            .map(|item| item.get("destination").unwrap())
            .collect();
        assert_eq!(
            destinations,
            vec![
                &json!("https://example.com/a"),
                &json!("https://example.com/b")
            ]
        );
        assert_eq!(
            found[0].get("rel"),
            Some(&Value::Array(vec![json!("nofollow"), json!("ugc")]))
        );
    }

    #[test]
    fn external_sponsored_link_is_not_an_internal_finding() {
        let edges = vec![edge_with(
            ROOT,
            "https://partner.example/",
            false,
            &["sponsored"],
            "",
            "",
        )];
        assert!(internal_sponsored_ugc_links(&edges, "example.com").is_empty());
    }

    #[test]
    fn internal_link_without_hint_tokens_is_silent() {
        let edges = vec![
            edge_with(ROOT, "https://example.com/a", false, &["nofollow"], "", ""),
            edge(ROOT, "https://example.com/b"),
        ];
        assert!(internal_sponsored_ugc_links(&edges, "example.com").is_empty());
    }

    #[test]
    fn unmeasured_rel_is_not_a_false_positive() {
        let edges = vec![edge(ROOT, "https://example.com/a")];
        assert!(internal_sponsored_ugc_links(&edges, "example.com").is_empty());
    }

    #[test]
    fn hint_token_match_is_exact_not_substring() {
        let edges = vec![edge_with(
            ROOT,
            "https://example.com/a",
            false,
            &["ugcx", "sponsoredish"],
            "",
            "",
        )];
        assert!(internal_sponsored_ugc_links(&edges, "example.com").is_empty());
    }

    #[test]
    fn page_linked_both_follow_and_nofollow_is_flagged() {
        let edges = vec![
            edge("https://example.com/a", "https://example.com/target"),
            edge_with(
                "https://example.com/b",
                "https://example.com/target",
                true,
                &[],
                "",
                "",
            ),
        ];
        assert_eq!(
            follow_and_nofollow_inlinks(&edges, "example.com"),
            vec!["https://example.com/target".to_string()]
        );
        assert_eq!(
            follow_and_nofollow_inlink_details(&edges, "example.com", 20),
            vec![details([
                ("target_url", json!("https://example.com/target")),
                (
                    "sources",
                    json!(["https://example.com/a", "https://example.com/b"]),
                ),
                ("follow_occurrences", json!(1)),
                ("nofollow_occurrences", json!(1)),
            ])]
        );
    }

    #[test]
    fn page_linked_only_followed_is_not_flagged() {
        let edges = vec![edge("https://example.com/a", "https://example.com/target")];
        assert!(follow_and_nofollow_inlinks(&edges, "example.com").is_empty());
    }

    #[test]
    fn external_destination_is_ignored() {
        let edges = vec![
            edge("https://example.com/a", "https://other.example/x"),
            edge_with(
                "https://example.com/b",
                "https://other.example/x",
                true,
                &[],
                "",
                "",
            ),
        ];
        assert!(follow_and_nofollow_inlinks(&edges, "example.com").is_empty());
    }

    #[test]
    fn host_match_is_case_insensitive() {
        let edges = vec![
            edge("https://example.com/a", "https://example.com/target"),
            edge_with(
                "https://example.com/b",
                "https://example.com/target",
                true,
                &[],
                "",
                "",
            ),
        ];
        assert_eq!(
            follow_and_nofollow_inlinks(&edges, "EXAMPLE.COM"),
            vec!["https://example.com/target".to_string()]
        );
    }

    #[test]
    fn http_action_is_flagged_regardless_of_page_scheme() {
        let forms = vec![form(ROOT, "post", "http://x.example/submit", false)];
        assert_eq!(
            form_url_insecure(&forms),
            vec![details([
                ("target_url", json!(ROOT)),
                ("action", json!("http://x.example/submit")),
                ("method", json!("post")),
            ])]
        );
    }

    #[test]
    fn https_action_is_not_flagged() {
        let forms = vec![form(ROOT, "post", "https://x.example/submit", false)];
        assert!(form_url_insecure(&forms).is_empty());
    }

    #[test]
    fn password_form_on_http_page_is_flagged() {
        let forms = vec![form("http://example.com/login", "post", "/login", true)];
        assert_eq!(
            forms_on_http_pages_with_password(&forms),
            vec![details([
                ("target_url", json!("http://example.com/login")),
                ("action", json!("/login")),
            ])]
        );
    }

    #[test]
    fn password_form_on_https_page_is_not_flagged() {
        let forms = vec![form("https://example.com/login", "post", "/login", true)];
        assert!(forms_on_http_pages_with_password(&forms).is_empty());
    }

    #[test]
    fn non_password_form_on_http_page_is_not_flagged() {
        let forms = vec![form("http://example.com/search", "get", "/s", false)];
        assert!(forms_on_http_pages_with_password(&forms).is_empty());
    }

    #[test]
    fn source_with_internal_nofollow_outlink_is_reported_with_its_destination() {
        let edges = vec![edge_with(
            "https://example.com/a",
            "https://example.com/b",
            true,
            &[],
            "",
            "",
        )];
        assert_eq!(
            nofollow_outlinks_items(&edges, "example.com", false, 20),
            vec![details([
                ("target_url", json!("https://example.com/a")),
                ("nofollow_occurrences", json!(1)),
                ("destinations", json!(["https://example.com/b"])),
            ])]
        );
    }

    #[test]
    fn mixed_follow_and_nofollow_source_counts_only_the_nofollow_edges() {
        let edges = vec![
            edge("https://example.com/a", "https://example.com/b"),
            edge_with(
                "https://example.com/a",
                "https://example.com/c",
                true,
                &[],
                "",
                "",
            ),
            edge_with(
                "https://example.com/a",
                "https://example.com/c",
                true,
                &[],
                "",
                "",
            ),
        ];
        let items = nofollow_outlinks_items(&edges, "example.com", false, 20);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("nofollow_occurrences"), Some(&json!(2)));
        assert_eq!(
            items[0].get("destinations"),
            Some(&Value::Array(vec![json!("https://example.com/c")]))
        );
    }

    #[test]
    fn source_with_only_followed_outlinks_is_not_reported() {
        let edges = vec![edge("https://example.com/a", "https://example.com/b")];
        assert!(nofollow_outlinks_items(&edges, "example.com", false, 20).is_empty());
    }

    #[test]
    fn external_nofollow_outlink_is_not_an_internal_finding() {
        let edges = vec![edge_with(
            "https://example.com/a",
            "https://other.test/",
            true,
            &[],
            "",
            "",
        )];
        assert!(nofollow_outlinks_items(&edges, "example.com", false, 20).is_empty());
    }

    #[test]
    fn destination_sample_is_bounded_but_the_count_is_not() {
        let edges: Vec<Edge> = (0..5)
            .map(|i| {
                edge_with(
                    "https://example.com/a",
                    &format!("https://example.com/{i}"),
                    true,
                    &[],
                    "",
                    "",
                )
            })
            .collect();
        let items = nofollow_outlinks_items(&edges, "example.com", false, 2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("nofollow_occurrences"), Some(&json!(5)));
        assert_eq!(
            items[0].get("destinations"),
            Some(&Value::Array(vec![
                json!("https://example.com/0"),
                json!("https://example.com/1"),
            ]))
        );
    }

    #[test]
    fn the_pure_filter_names_source_and_blocked_targets() {
        let hidden = "https://example.com/hidden";
        let open = "https://example.com/open";
        let edges = vec![edge(ROOT, hidden), edge(ROOT, open), edge(open, hidden)];
        let blocked: BTreeSet<String> = [hidden.to_string()].into();
        assert_eq!(
            robots_blocked_outlinks(&edges, &blocked),
            vec![
                details([
                    ("target_url", json!(ROOT)),
                    ("count", json!(1)),
                    ("blocked_targets", json!([hidden])),
                ]),
                details([
                    ("target_url", json!(open)),
                    ("count", json!(1)),
                    ("blocked_targets", json!([hidden])),
                ]),
            ]
        );
    }

    #[test]
    fn the_pure_filter_quotes_five_blocked_targets_and_deduplicates() {
        let mut edges: Vec<Edge> = (0..7)
            .map(|n| edge(ROOT, &format!("{ROOT}hidden/{n}")))
            .collect();
        edges.push(edge(ROOT, &format!("{ROOT}hidden/0")));
        let blocked: BTreeSet<String> = (0..7).map(|n| format!("{ROOT}hidden/{n}")).collect();
        let items = robots_blocked_outlinks(&edges, &blocked);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("count"), Some(&json!(8)));
        assert_eq!(
            items[0].get("blocked_targets"),
            Some(&Value::Array(
                (0..5).map(|n| json!(format!("{ROOT}hidden/{n}"))).collect()
            ))
        );
    }

    #[test]
    fn mixed_qualifying_tokens_are_valid_and_preserve_all_tokens() {
        let edges = vec![
            edge_with(
                ROOT,
                OUT,
                true,
                &["SPONSORED", "nofollow", "noopener", "sponsored"],
                "",
                "",
            ),
            edge_with(ROOT, OUT, true, &["ugcx", "nofollow"], "", ""),
        ];
        assert_eq!(
            mixed_rel_links(&edges),
            vec![details([
                ("target_url", json!(ROOT)),
                ("destination", json!(OUT)),
                ("rel", json!(["nofollow", "noopener", "sponsored"])),
                ("qualifying_rel", json!(["nofollow", "sponsored"])),
                ("combination_valid", json!(true)),
            ])]
        );
        let summary = rel_summary(&edges);
        assert_eq!(summary.get("counts_overlap"), Some(&json!(true)));
        assert_eq!(
            summary.get("by_rel"),
            Some(&Value::Object(details([
                ("total", json!(2)),
                ("nofollow", json!(2)),
                ("sponsored", json!(1)),
                ("ugc", json!(0)),
                ("mixed", json!(1)),
                ("unqualified", json!(0)),
            ])))
        );
    }

    #[test]
    fn local_disclosure_labels_are_recognized() {
        for label in [
            "Sponsored",
            "Paid partnership",
            "Advertisement",
            "Affiliate link",
            "Реклама",
            "Партнёрский материал",
        ] {
            let html =
                format!("<section><p>{label}</p><p><a href=\"{OUT}\" rel=\"sponsored\">Offer</a></p></section>");
            let ctx =
                spiderhead_parse::link_context::extract_occurrences(&html, ROOT, None, None, 20000)
                    .unwrap();
            assert_eq!(ctx.occurrences.len(), 1);
            assert_eq!(
                ctx.occurrences[0]
                    .disclosure
                    .as_ref()
                    .and_then(|d| d.get("detected"))
                    .and_then(Value::as_bool),
                Some(true),
                "{label}"
            );
        }
    }

    #[test]
    fn inert_or_explicitly_hidden_text_is_not_disclosure() {
        for label in [
            "<span hidden>Sponsored</span>",
            "<span aria-hidden=\"true\">Sponsored</span>",
            "<span style=\"display: none\">Sponsored</span>",
            "<template>Sponsored</template>",
            "<script>Sponsored</script>",
        ] {
            let html = format!(
                "<p>{label}<a href=\"{OUT}\" rel=\"sponsored\">Offer</a></p><footer>Sponsored links</footer>"
            );
            let ctx =
                spiderhead_parse::link_context::extract_occurrences(&html, ROOT, None, None, 20000)
                    .unwrap();
            assert_eq!(ctx.occurrences.len(), 1);
            assert_eq!(
                ctx.occurrences[0]
                    .disclosure
                    .as_ref()
                    .and_then(|d| d.get("detected"))
                    .and_then(Value::as_bool),
                Some(false),
                "{label}"
            );
        }
    }
}
