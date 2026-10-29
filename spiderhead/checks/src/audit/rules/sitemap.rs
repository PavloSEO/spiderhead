//! Retained sitemap and robots findings (reference the reference suite over
//! the reference suite's reconciliation), computed in memory from the stored scan.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::net::normalize_url;
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::py_url::urlsplit;

use super::stored::SELECTED_LINKS;
use super::{details, round_to};
use crate::audit::load::same_page;
use crate::audit::robots::{is_allowed, match_path};
use crate::audit::{norm_url, AuditContext};

const STATUS_CHECKS: [&str; 5] = [
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_NON_INDEXABLE",
    "SITEMAP_URL_NON_CANONICAL",
    "SITEMAP_URL_BLOCKED_ROBOTS",
];
const COVERAGE_CHECKS: [&str; 3] = ["SITEMAP_ORPHAN", "URL_NOT_IN_SITEMAP", "SITEMAP_DESYNC"];

struct PageRow {
    url_id: i64,
    url: String,
    status: Option<i64>,
    content_type: String,
    meta_robots: String,
    x_robots: String,
    canonical: String,
    redirect_url: String,
}

fn netloc(url: &str) -> String {
    urlsplit(url)
        .map(|s| s.netloc.to_lowercase())
        .unwrap_or_default()
}

fn scheme(url: &str) -> String {
    urlsplit(url)
        .map(|s| s.scheme.to_lowercase())
        .unwrap_or_default()
}

fn blocked(robots: Option<&Value>, target: &str, start: &str) -> bool {
    let Some(r) = robots else { return false };
    netloc(target) == netloc(start)
        && scheme(target) == scheme(start)
        && !is_allowed(
            &r["parsed"],
            &match_path(target),
            r["token"].as_str().unwrap_or("*"),
        )
}

fn query_all<T>(
    con: &Connection,
    sql: &str,
    f: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Vec<T> {
    con.prepare(sql)
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| f(r))
                .ok()
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
}

fn payloads(con: &Connection, kind: &str) -> Vec<Value> {
    query_all(
        con,
        &format!("SELECT payload_json FROM context_items WHERE kind='{kind}' ORDER BY item_key"),
        |r| r.get::<_, String>(0),
    )
    .into_iter()
    .filter_map(|t| serde_json::from_str(&t).ok())
    .collect()
}

/// Declarations, robots policy and per-URL sitemap verdicts of a stored scan.
/// `run_retained_sitemap` (the reference suite): emits the findings and returns the
/// `summary.sitemap` block (`measured`), including the collection-bound URL lists.
pub fn run_retained_sitemap(ctx: &mut AuditContext) -> Map<String, Value> {
    let Some(con) = ctx.scan.take() else {
        return Map::new();
    };
    let start: String = con
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let robots: Option<Value> = payloads(&con, "robots_summary")
        .into_iter()
        .next()
        .filter(|p| p["fetch_state"] == "fetched");
    let url_of: HashMap<i64, String> = query_all(&con, "SELECT url_id,url FROM urls", |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .into_iter()
    .collect();
    for check in [
        "SITEMAP_TOO_MANY_URLS",
        "SITEMAP_TOO_LARGE",
        "SITEMAP_URL_DUPLICATED",
        "SITEMAP_STALE_LASTMOD",
    ] {
        ctx.skip(
            check,
            "retained sitemap membership does not include protocol size or lastmod evidence",
        );
    }
    match &robots {
        None => ctx.skip(
            "SITEMAP_NOT_IN_ROBOTS",
            "no retained fetched robots.txt policy",
        ),
        Some(r) if r["parsed"]["sitemaps"].as_array().is_none_or(Vec::is_empty) => ctx.add(
            "SITEMAP_NOT_IN_ROBOTS",
            Some(&start),
            details([
                ("reason", "retained robots.txt declares no sitemap".into()),
                ("evidence_source", "retained_scan".into()),
            ]),
        ),
        _ => {}
    }
    run_robots(ctx, &con, robots.as_ref(), &start, &url_of);

    let mut declarations = payloads(&con, "sitemap_declaration");
    declarations.sort_by_key(|p| p["ordinal"].as_i64().unwrap_or(0));
    let roots: Vec<String> = declarations
        .iter()
        .filter_map(|p| url_of.get(&p["sitemap_url_id"].as_i64()?).cloned())
        .collect();
    let mut summary = details([
        ("evidence_source", "retained_scan".into()),
        ("offline", json!(true)),
        (
            "sitemap_url",
            roots.first().map_or(Value::Null, |u| json!(u)),
        ),
        ("sitemap_urls", json!(roots)),
    ]);
    if declarations.is_empty() {
        let reason = "no saved sitemap declarations";
        for check in STATUS_CHECKS
            .iter()
            .chain(COVERAGE_CHECKS.iter())
            .chain(["SITEMAP_FETCH_INCOMPLETE"].iter())
        {
            ctx.skip(check, reason);
        }
        summary.insert("reconciliation_available".into(), json!(false));
        summary.insert("reconciliation_reason".into(), reason.into());
        ctx.scan = Some(con);
        return summary;
    }

    // membership
    let mut members: Vec<(i64, i64, i64)> = payloads(&con, "sitemap_declared_url")
        .iter()
        .filter_map(|p| {
            Some((
                p["ordinal"].as_i64()?,
                p["sitemap_url_id"].as_i64()?,
                p["url_id"].as_i64()?,
            ))
        })
        .collect();
    members.sort();
    let summaries: HashMap<i64, bool> = payloads(&con, "sitemap_fetch_summary")
        .iter()
        .filter_map(|p| Some((p["sitemap_url_id"].as_i64()?, p["complete"].as_bool()?)))
        .collect();
    let sources: BTreeSet<i64> = declarations
        .iter()
        .filter_map(|p| p["sitemap_url_id"].as_i64())
        .collect();
    let complete = sources.iter().all(|s| summaries.get(s) == Some(&true));
    let mut declared: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for (_, sitemap, url_id) in &members {
        if let Some(url) = url_of.get(url_id) {
            declared
                .entry(normalize_url(url))
                .or_insert((*url_id, *sitemap));
        }
    }
    let blocked_ids: HashSet<i64> = payloads(&con, "robots_blocked_url")
        .iter()
        .filter_map(|p| p["url_id"].as_i64())
        .collect();
    let pages: Vec<PageRow> = query_all(
        &con,
        "SELECT p.url_id,u.url,p.status_code,p.content_type,p.meta_robots,p.x_robots,p.canonical,p.redirect_url FROM pages p JOIN urls u ON u.url_id=p.url_id ORDER BY p.page_ordinal",
        |r| Ok(PageRow { url_id: r.get(0)?, url: r.get(1)?, status: r.get(2)?, content_type: r.get(3)?, meta_robots: r.get(4)?, x_robots: r.get(5)?, canonical: r.get(6)?, redirect_url: r.get(7)? }),
    );
    let start_host = netloc(&start);
    let mut page_key: HashMap<String, usize> = HashMap::new();
    let mut comparable: BTreeMap<String, i64> = BTreeMap::new();
    for (i, p) in pages.iter().enumerate() {
        let key = normalize_url(&p.url);
        page_key.entry(key.clone()).or_insert(i);
        if blocked_ids.contains(&p.url_id)
            || !p.status.is_some_and(|s| (200..300).contains(&s))
            || !p.content_type.to_lowercase().contains("html")
        {
            continue;
        }
        if !start_host.is_empty() && netloc(&p.url) != start_host {
            continue;
        }
        if robots_directives(&[Some(&p.meta_robots), Some(&p.x_robots)])
            .iter()
            .any(|d| d == "noindex")
        {
            continue;
        }
        if !p.canonical.is_empty() && !same_page(&p.url, &p.canonical) {
            continue;
        }
        comparable.entry(key).or_insert(p.url_id);
    }

    // per-URL declared facts
    let mut unmeasured = 0;
    for (key, (url_id, sitemap_id)) in &declared {
        let target = &url_of[url_id];
        let page = page_key.get(key).map(|&i| &pages[i]);
        let status = page.and_then(|p| p.status);
        let sitemap_url = url_of.get(sitemap_id).cloned().unwrap_or_default();
        let base = |extra: &[(&str, Value)]| {
            let mut m = details([
                ("in_sitemap", json!(true)),
                ("source_url", json!(sitemap_url)),
                ("target_url", json!(target)),
                ("status_code", json!(status)),
                ("evidence_source", json!("retained_scan")),
            ]);
            for (k, v) in extra {
                m.insert((*k).to_owned(), v.clone());
            }
            m
        };
        let evidence = details([
            ("sitemap_url", json!(sitemap_url)),
            ("retained", json!(true)),
        ]);
        // The oracle stamps `evidence` on every per-URL sitemap finding.
        let stamped = |ctx: &mut AuditContext, added: bool| {
            if added {
                if let Some(issue) = ctx.issues.last_mut() {
                    issue.evidence = evidence.clone();
                }
            }
        };
        if blocked_ids.contains(url_id) || blocked(robots.as_ref(), target, &start) {
            let added = ctx.add_with(
                "SITEMAP_URL_BLOCKED_ROBOTS",
                Some(target),
                base(&[("reason", json!("blocked by retained robots.txt policy"))]),
                status,
                1,
                None,
            );
            stamped(ctx, added);
        }
        let Some(status) = status else {
            unmeasured += 1;
            continue;
        };
        let p = page.expect("status implies a page");
        match status {
            300..=399 => {
                let added = ctx.add_with(
                    "SITEMAP_URL_3XX",
                    Some(target),
                    base(&[
                        ("reason", json!("redirect")),
                        ("redirect_url", json!(p.redirect_url)),
                    ]),
                    Some(status),
                    1,
                    None,
                );
                stamped(ctx, added);
            }
            400..=599 => {
                let added = ctx.add_with(
                    "SITEMAP_URL_4XX_5XX",
                    Some(target),
                    base(&[(
                        "reason",
                        json!(if status < 500 {
                            "client error"
                        } else {
                            "server error"
                        }),
                    )]),
                    Some(status),
                    1,
                    None,
                );
                stamped(ctx, added);
            }
            200..=299 => {
                if robots_directives(&[Some(&p.meta_robots), Some(&p.x_robots)])
                    .iter()
                    .any(|d| d == "noindex")
                {
                    let added = ctx.add_with(
                        "SITEMAP_URL_NON_INDEXABLE",
                        Some(target),
                        base(&[
                            ("reason", json!("noindex")),
                            ("indexability", json!("Non-Indexable")),
                        ]),
                        Some(status),
                        1,
                        None,
                    );
                    stamped(ctx, added);
                }
                if !p.canonical.is_empty() && norm_url(target) != norm_url(&p.canonical) {
                    let added = ctx.add_with(
                        "SITEMAP_URL_NON_CANONICAL",
                        Some(target),
                        base(&[
                            ("reason", json!("canonical points to another URL")),
                            ("canonical_url", json!(p.canonical)),
                        ]),
                        Some(status),
                        1,
                        None,
                    );
                    stamped(ctx, added);
                }
            }
            _ => {}
        }
    }
    if unmeasured > 0 {
        for check in &STATUS_CHECKS[..4] {
            ctx.skip(
                check,
                &format!("{unmeasured} declared sitemap URL(s) have no retained response"),
            );
        }
    }
    if robots.is_none() {
        ctx.skip(
            "SITEMAP_URL_BLOCKED_ROBOTS",
            "no retained fetched robots.txt policy",
        );
    }
    summary.insert("sitemap_status_unmeasured".into(), unmeasured.into());
    if !complete {
        let reason = "saved sitemap declaration membership is partial";
        ctx.add(
            "SITEMAP_FETCH_INCOMPLETE",
            Some(&start),
            details([
                ("reason", reason.into()),
                ("evidence_source", "retained_scan".into()),
            ]),
        );
        for check in COVERAGE_CHECKS {
            ctx.skip(check, reason);
        }
        summary.insert("reconciliation_available".into(), json!(false));
        summary.insert("reconciliation_reason".into(), reason.into());
        ctx.scan = Some(con);
        return summary;
    }

    // observed link targets
    let sql = format!("{SELECTED_LINKS}SELECT l.destination_url_id,destination.url FROM l JOIN pages AS sp ON sp.url_id=l.source_url_id JOIN urls AS destination ON destination.url_id=l.destination_url_id ORDER BY sp.page_ordinal,l.ordinal,l.link_id");
    let mut observed: BTreeMap<String, i64> = BTreeMap::new();
    for (id, url) in query_all(&con, &sql, |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    }) {
        observed.entry(normalize_url(&url)).or_insert(id);
    }
    let crawl_partial: bool = con
        .query_row(
            "SELECT crawl_partial FROM scan WHERE singleton=1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false);
    let in_sitemap_not_linked: Vec<&String> = declared
        .keys()
        .filter(|k| !observed.contains_key(*k))
        .collect();
    let linked_not_in_sitemap: Vec<&String> = comparable
        .keys()
        .filter(|k| observed.contains_key(*k) && !declared.contains_key(*k))
        .collect();
    let linked_not_comparable: Vec<&String> = observed
        .keys()
        .filter(|k| !comparable.contains_key(*k) && !declared.contains_key(*k))
        .collect();
    let in_and_linked = declared.len() - in_sitemap_not_linked.len();
    let example = |keys: &[&String], map: &dyn Fn(&String) -> i64| -> Vec<String> {
        keys.iter()
            .take(20)
            .map(|k| url_of[&map(k)].clone())
            .collect()
    };
    let ex_orphan = example(&in_sitemap_not_linked, &|k| declared[k].0);
    let ex_missing = example(&linked_not_in_sitemap, &|k| comparable[k]);
    let ex_not_comparable = example(&linked_not_comparable, &|k| observed[k]);
    let ex_and_linked: Vec<String> = declared
        .keys()
        .filter(|k| observed.contains_key(*k))
        .take(20)
        .map(|k| url_of[&observed[k]].clone())
        .collect();
    summary.insert("available".into(), json!(true));
    summary.insert("reason".into(), "".into());
    summary.insert("urls_in_sitemap".into(), (declared.len() as i64).into());
    summary.insert(
        "urls_reached_by_links".into(),
        (observed.len() as i64).into(),
    );
    summary.insert(
        "in_sitemap_not_in_crawl".into(),
        (in_sitemap_not_linked.len() as i64).into(),
    );
    summary.insert(
        "in_crawl_not_in_sitemap".into(),
        (linked_not_in_sitemap.len() as i64).into(),
    );
    summary.insert("crawl_partial".into(), json!(crawl_partial));
    summary.insert("reconciliation_available".into(), json!(true));
    summary.insert("in_sitemap_and_linked".into(), json!(ex_and_linked));
    summary.insert(
        "in_sitemap_and_linked_count".into(),
        ((declared.len() - in_sitemap_not_linked.len()) as i64).into(),
    );
    summary.insert("in_sitemap_not_linked".into(), json!(ex_orphan.clone()));
    summary.insert(
        "in_sitemap_not_linked_count".into(),
        (in_sitemap_not_linked.len() as i64).into(),
    );
    summary.insert("linked_not_in_sitemap".into(), json!(ex_missing.clone()));
    summary.insert(
        "linked_not_in_sitemap_count".into(),
        (linked_not_in_sitemap.len() as i64).into(),
    );
    summary.insert("linked_not_comparable".into(), json!(ex_not_comparable));
    summary.insert(
        "linked_not_comparable_count".into(),
        (linked_not_comparable.len() as i64).into(),
    );
    summary.insert("examples_limit".into(), (20i64).into());

    let mut missing = 0;
    for key in comparable.keys().filter(|k| !declared.contains_key(*k)) {
        let url = &url_of[&comparable[key]];
        if blocked(robots.as_ref(), url, &start) {
            continue;
        }
        missing += 1;
        let status = page_key.get(key).and_then(|&i| pages[i].status);
        ctx.add_with(
            "URL_NOT_IN_SITEMAP",
            Some(url),
            details([
                ("source_url", json!(roots.first())),
                ("target_url", json!(url)),
                ("status_code", json!(status)),
                (
                    "reason",
                    "canonical indexable HTML page absent from retained sitemap".into(),
                ),
                ("evidence_source", "retained_scan".into()),
            ]),
            status,
            1,
            None,
        );
    }
    summary.insert(
        "canonical_pages_not_in_sitemap_count".into(),
        (missing as i64).into(),
    );
    if crawl_partial {
        ctx.skip(
            "SITEMAP_ORPHAN",
            "crawl is partial: internal link absence cannot be proven",
        );
        ctx.skip(
            "SITEMAP_DESYNC",
            "crawl is partial: sitemap-versus-link-graph verdict cannot be proven",
        );
    } else {
        let mut orphaned = 0usize;
        for key in &in_sitemap_not_linked {
            let (url_id, sitemap_id) = declared[*key];
            let target = &url_of[&url_id];
            if norm_url(target) == norm_url(&start) {
                continue;
            }
            orphaned += 1;
            let status = page_key.get(*key).and_then(|&i| pages[i].status);
            ctx.add_with(
                "SITEMAP_ORPHAN",
                Some(target),
                details([
                    ("in_sitemap", json!(true)),
                    ("source_url", json!(url_of.get(&sitemap_id))),
                    ("target_url", json!(target)),
                    ("status_code", json!(status)),
                    ("reason", "no retained internal link".into()),
                    ("evidence_source", "retained_scan".into()),
                ]),
                status,
                1,
                None,
            );
        }
        let linked_missing = linked_not_in_sitemap.len();
        summary.insert(
            "sitemap_orphan_findings_count".into(),
            (orphaned as i64).into(),
        );
        let crawl_pct = round_to(
            100.0 * linked_missing as f64 / (in_and_linked + linked_missing).max(1) as f64,
            1,
        );
        let sitemap_pct = round_to(100.0 * orphaned as f64 / declared.len().max(1) as f64, 1);
        let warn_pct = ctx
            .thresholds
            .get("sitemap_desync_pct_warn")
            .unwrap_or(20.0);
        if crawl_pct.max(sitemap_pct) >= warn_pct {
            let d: Map<String, Value> = details([
                ("in_crawl_not_in_sitemap", linked_missing.into()),
                ("in_sitemap_not_in_crawl", orphaned.into()),
                ("crawl_not_in_sitemap_pct", Value::from(crawl_pct)),
                ("sitemap_not_in_crawl_pct", Value::from(sitemap_pct)),
                ("examples_missing_from_sitemap", json!(ex_missing)),
                ("examples_in_sitemap_not_crawled", json!(ex_orphan)),
                ("evidence_source", "retained_scan".into()),
            ]);
            ctx.add("SITEMAP_DESYNC", Some(&start), d);
        }
    }
    ctx.scan = Some(con);
    summary
}

fn run_robots(
    ctx: &mut AuditContext,
    con: &Connection,
    robots: Option<&Value>,
    start: &str,
    url_of: &HashMap<i64, String>,
) {
    let sql = format!(
        "{SELECTED_LINKS}SELECT src.url,dst.url,p.status_code,c.payload_json IS NOT NULL FROM l \
         JOIN urls src ON src.url_id=l.source_url_id JOIN urls dst ON dst.url_id=l.destination_url_id \
         LEFT JOIN pages p ON p.url_id=l.destination_url_id \
         LEFT JOIN context_items c ON c.kind='robots_blocked_url' AND c.item_key='url:'||dst.url_id \
         ORDER BY l.source_url_id,l.ordinal,l.link_id"
    );
    let mut explicit = false;
    for (source, target, status, flagged) in query_all(con, &sql, |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, i64>(3)? != 0,
        ))
    }) {
        if netloc(&target) != netloc(start) {
            continue;
        }
        if flagged || blocked(robots, &target, start) {
            explicit = explicit || flagged;
            ctx.add_located(
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                &target,
                status,
                1,
                vec![json!({"source_url": source, "destination_url": target})],
                details([
                    ("source_url", json!(source)),
                    ("target_url", json!(target)),
                    ("status_code", json!(status)),
                    (
                        "reason",
                        "internal link targets a robots-blocked URL".into(),
                    ),
                    ("evidence_source", "retained_scan".into()),
                ]),
            );
            if let Some(issue) = ctx.issues.last_mut() {
                issue.evidence = details([("retained_robots", true.into())]);
            }
        }
    }
    let _ = url_of;
    let refs = query_all(
        con,
        "SELECT src.url,dst.url,r.kind,r.capture_state,r.reason FROM resource_refs r JOIN pages p ON p.url_id=r.page_url_id \
         JOIN urls src ON src.url_id=r.page_url_id JOIN urls dst ON dst.url_id=r.resource_url_id \
         WHERE r.representation=p.representation AND r.kind IN ('script','stylesheet') ORDER BY r.page_url_id,r.ordinal",
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)),
    );
    for (source, target, kind, state, reason) in refs {
        if state == "excluded_robots" || blocked(robots, &target, start) {
            ctx.add(
                "ROBOTS_BLOCKS_RESOURCES",
                Some(&source),
                details([
                    ("source_url", json!(source)),
                    ("target_url", json!(target)),
                    ("kind", json!(kind)),
                    (
                        "reason",
                        "rendering dependency blocked by retained robots.txt".into(),
                    ),
                    ("capture_reason", json!(reason)),
                    ("evidence_source", "retained_scan".into()),
                ]),
            );
            if let Some(issue) = ctx.issues.last_mut() {
                issue.evidence = details([("retained_resource_declaration", true.into())]);
            }
        }
    }
    if robots.is_none() {
        if !explicit {
            ctx.skip(
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                "no retained fetched robots.txt policy",
            );
        }
        ctx.skip(
            "ROBOTS_BLOCKS_RESOURCES",
            "no retained fetched robots.txt policy or blocked dependency",
        );
    } else {
        let any_ref: bool = con
            .query_row("SELECT 1 FROM resource_refs LIMIT 1", [], |_| Ok(()))
            .is_ok();
        let inventory: bool = con.query_row("SELECT 1 FROM context_items WHERE kind='resource_inventory' AND completeness='complete' LIMIT 1", [], |_| Ok(())).is_ok();
        if !any_ref && !inventory {
            ctx.skip(
                "ROBOTS_BLOCKS_RESOURCES",
                "no retained script or stylesheet dependency inventory",
            );
        }
    }
}
