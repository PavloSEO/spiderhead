//! `run_inlinks` whole-graph checks over the stored `AnalysisGraph`: link score,
//! inlink composition, discovery paths, and the internal-linking summary
//! (`sf/core/inlinks/`).

use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};
use spiderhead_store::analysis_graph::AnalysisGraph;

use crate::audit::context::AuditContext;
use crate::audit::model::Page;
use crate::audit::rules::details;

/// `DEFAULT_CLICK_DEPTH_FLOOR` (the reference suite): the coarser navigation floor, not
/// the crawl's own `crawl_depth_max` budget.
const CLICK_DEPTH_FLOOR: i64 = 10;
/// `_MAX_DUPLICATE_REPEATS`.
const MAX_DUPLICATE_REPEATS: i64 = 20;
/// `_MAX_PATH_URLS`: routes longer than this are reported by their two ends.
const MAX_PATH_URLS: i64 = 20;
/// `_PATH_END_URLS`.
const PATH_END_URLS: usize = 5;
const DEPTH_MARKS: [i64; 3] = [3, 5, 10];

fn norm(url: &str) -> String {
    crate::audit::norm_url(url)
}

fn rec(page: &Page) -> &Map<String, Value> {
    &page.rec
}

/// One repeated-link pattern bucket (`_emit_duplicate_links`).
type DupBucket = (Vec<Map<String, Value>>, i64, Vec<String>);

/// `_site_host` (the reference suite): the most common netloc across crawled pages.
fn site_host(ctx: &AuditContext) -> String {
    let mut counts: std::collections::HashMap<String, i64> = Default::default();
    for page in &ctx.pages {
        if let Ok(s) = spiderhead_parse::py_url::urlsplit(&page.url) {
            *counts.entry(s.netloc.to_lowercase()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(h, _)| h)
        .unwrap_or_default()
}

/// `internal_linking.unmeasured`.
fn unmeasured(reason: &str) -> Map<String, Value> {
    details([("measured", Value::from(false)), ("reason", reason.into())])
}

/// `summarize_positions` (the reference suite).
fn summarize_positions(totals: &Map<String, Value>) -> Map<String, Value> {
    let mut by_position = Map::new();
    let mut unclassified = 0i64;
    for (position, count) in totals {
        let n = count.as_i64().unwrap_or(0);
        if position.is_empty() {
            unclassified = n;
        } else if n > 0 {
            by_position.insert(position.clone(), Value::from(n));
        }
    }
    let classified: i64 = by_position.values().map(|v| v.as_i64().unwrap_or(0)).sum();
    let edges_total = classified + unclassified;
    let mut out = Map::new();
    out.insert("edges_total".into(), Value::from(edges_total));
    out.insert("by_position".into(), Value::Object(by_position));
    out.insert("unclassified".into(), Value::from(unclassified));
    out.insert(
        "classified_fraction".into(),
        Value::from(if edges_total > 0 {
            (classified as f64 / edges_total as f64 * 10000.0).round() / 10000.0
        } else {
            0.0
        }),
    );
    out.insert(
        "positions_note".into(),
        Value::from(
            "edges with no recorded position are counted under 'unclassified', never \
             as content; re-crawl with link_position.classify to classify them",
        ),
    );
    out
}

/// `summarize_depth` (the reference suite) over the graph `depth_for` lookup.
fn summarize_depth(
    depths: &dyn Fn(&str) -> Option<i64>,
    page_keys: &[String],
    seed: &str,
    floor: i64,
) -> Map<String, Value> {
    let mut histogram: std::collections::BTreeMap<i64, i64> = Default::default();
    let (mut reached, mut deepest) = (0i64, 0i64);
    for key in page_keys {
        let Some(depth) = depths(key) else { continue };
        reached += 1;
        deepest = deepest.max(depth);
        *histogram.entry(depth).or_default() += 1;
    }
    let pages = page_keys.len() as i64;
    let mut within = Map::new();
    for mark in DEPTH_MARKS {
        let n: i64 = histogram
            .iter()
            .filter(|(d, _)| **d <= mark)
            .map(|(_, c)| *c)
            .sum();
        within.insert(mark.to_string(), Value::from(n));
    }
    let mut out = Map::new();
    out.insert("measured".into(), Value::from(true));
    out.insert("seed".into(), Value::from(seed));
    out.insert("pages".into(), Value::from(pages));
    out.insert("reachable".into(), Value::from(reached));
    out.insert("unreachable".into(), Value::from(pages - reached));
    out.insert(
        "max".into(),
        if reached > 0 {
            Value::from(deepest)
        } else {
            Value::Null
        },
    );
    out.insert(
        "histogram".into(),
        Value::Object(
            histogram
                .into_iter()
                .map(|(d, c)| (d.to_string(), Value::from(c)))
                .collect(),
        ),
    );
    out.insert("within".into(), Value::Object(within));
    out.insert("floor_used".into(), Value::from(floor));
    out.insert("edges".into(), Value::from("internal followed hyperlinks"));
    out
}

/// `click_depth_seed`: the recorded crawl start URL; native scans always carry one.
fn click_depth_seed(ctx: &AuditContext) -> (Option<String>, String) {
    if let Some(start) = ctx.start_url.as_deref().filter(|s| !s.is_empty()) {
        return (Some(norm(start)), String::new());
    }
    let roots: Vec<&Page> = ctx
        .pages
        .iter()
        .filter(|p| rec(p).get("crawl_depth").and_then(Value::as_i64) == Some(0))
        .collect();
    match roots.len() {
        0 => (
            None,
            "the run recorded no crawl start URL and no page is at Crawl Depth 0, so \
             there is no page to walk the link graph from"
                .to_owned(),
        ),
        1 => (Some(norm(&roots[0].url)), String::new()),
        n => (
            None,
            format!(
                "the run recorded no crawl start URL and {n} pages carry Crawl Depth 0, \
                 so that column cannot identify where the crawl began"
            ),
        ),
    }
}

/// `check_link_score` (graph path): writes `link_score_computed` per page and fires
/// `LOW_LINK_SCORE` for indexable pages far under the site median.
pub fn check_link_score(ctx: &mut AuditContext) {
    if !ctx.graph_access {
        ctx.skip(
            "LOW_LINK_SCORE",
            "no all_inlinks export (needed for the complete internal edge list)",
        );
        return;
    }
    let Some(con) = ctx.scan.take() else {
        ctx.skip(
            "LOW_LINK_SCORE",
            "no all_inlinks export (needed for the complete internal edge list)",
        );
        return;
    };
    let host = site_host(ctx);
    let urls: Vec<String> = ctx.pages.iter().map(|p| p.url.clone()).collect();
    let scored = {
        let created = AnalysisGraph::new(&con, |u: &str| norm(u), &host);
        created.ok().and_then(|mut graph| {
            graph
                .link_score(0.85, 200, 1e-10)
                .ok()
                .flatten()
                .map(|view| {
                    let mut scores: Vec<(String, f64)> = Vec::with_capacity(urls.len());
                    for url in &urls {
                        if let Ok(Some(score)) = view.score_for(&norm(url)) {
                            scores.push((url.clone(), score));
                        }
                    }
                    (scores, view.count, view.median)
                })
        })
    };
    ctx.scan = Some(con);
    let Some((scores, count, median)) = scored else {
        ctx.skip(
            "LOW_LINK_SCORE",
            "all_inlinks export has no internal followed hyperlinks",
        );
        return;
    };
    let by_url: std::collections::HashMap<&str, f64> =
        scores.iter().map(|(u, s)| (u.as_str(), *s)).collect();
    for page in &mut ctx.pages {
        if let Some(score) = by_url.get(page.url.as_str()) {
            let rounded = (score * 1e6).round() / 1e6;
            page.metrics
                .insert("link_score_computed".into(), Value::from(rounded));
        }
    }
    if count < 5 || median <= 0.0 {
        return;
    }
    let ratio = ctx.thresholds.link_score_low_ratio;
    let mut fired: Vec<(String, f64)> = Vec::new();
    for i in ctx.indexable_html_pages() {
        let page = &ctx.pages[i];
        if rec(page).get("crawl_depth").and_then(Value::as_i64) == Some(0) {
            continue;
        }
        let Some(score) = by_url.get(page.url.as_str()) else {
            continue;
        };
        if *score >= median * ratio {
            continue;
        }
        fired.push((page.url.clone(), *score));
    }
    for (url, score) in fired {
        ctx.add(
            "LOW_LINK_SCORE",
            Some(&url),
            details([
                ("link_score", Value::from((score * 1e6).round() / 1e6)),
                ("site_median", Value::from((median * 1e6).round() / 1e6)),
                (
                    "ratio_to_median",
                    Value::from((score / median * 1000.0).round() / 1000.0),
                ),
            ]),
        );
    }
}

/// `check_inlink_composition` (graph path): `ONLY_NOFOLLOW_INLINKS` and
/// `ONLY_NONINDEXABLE_SOURCE_INLINKS`.
pub fn check_inlink_composition(ctx: &mut AuditContext) {
    if !ctx.graph_access {
        for check in ["ONLY_NOFOLLOW_INLINKS", "ONLY_NONINDEXABLE_SOURCE_INLINKS"] {
            ctx.skip(
                check,
                "no all_inlinks export (needed for the complete inlink list)",
            );
        }
        return;
    }
    let Some(con) = ctx.scan.take() else {
        for check in ["ONLY_NOFOLLOW_INLINKS", "ONLY_NONINDEXABLE_SOURCE_INLINKS"] {
            ctx.skip(
                check,
                "no all_inlinks export (needed for the complete inlink list)",
            );
        }
        return;
    };
    let by_norm: std::collections::HashMap<String, bool> = ctx
        .pages
        .iter()
        .map(|p| (norm(&p.url), p.is_indexable()))
        .collect();
    let host = site_host(ctx);
    let rows = {
        let created = AnalysisGraph::new(&con, |u: &str| norm(u), &host);
        created.ok().map(|mut graph| {
            graph
                .iter_inlink_composition(|source| by_norm.get(&norm(source)).copied(), 20)
                .unwrap_or_default()
        })
    };
    ctx.scan = Some(con);
    let Some(rows) = rows else {
        for check in ["ONLY_NOFOLLOW_INLINKS", "ONLY_NONINDEXABLE_SOURCE_INLINKS"] {
            ctx.skip(
                check,
                "no all_inlinks export (needed for the complete inlink list)",
            );
        }
        return;
    };
    for row in rows {
        let Some(&target_indexable) = by_norm.get(&row.destination_key) else {
            continue;
        };
        if !target_indexable {
            continue;
        }
        let Some(i) = ctx
            .pages
            .iter()
            .position(|p| norm(&p.url) == row.destination_key)
        else {
            continue;
        };
        if rec(&ctx.pages[i])
            .get("crawl_depth")
            .and_then(Value::as_i64)
            == Some(0)
        {
            continue;
        }
        let target = ctx.pages[i].url.clone();
        if row.all_nofollow {
            ctx.add_with(
                "ONLY_NOFOLLOW_INLINKS",
                Some(&target),
                details([
                    ("inlink_count", Value::from(row.occurrences_count as i64)),
                    (
                        "sources",
                        Value::Array(
                            row.source_examples
                                .iter()
                                .map(|u| Value::from(u.clone()))
                                .collect(),
                        ),
                    ),
                ]),
                None,
                row.occurrences_count as i64,
                None,
            );
        }
        if row.has_known_source && !row.has_indexable_source {
            ctx.add_with(
                "ONLY_NONINDEXABLE_SOURCE_INLINKS",
                Some(&target),
                details([
                    ("inlink_count", Value::from(row.occurrences_count as i64)),
                    (
                        "sources",
                        Value::Array(
                            row.source_examples
                                .iter()
                                .map(|u| Value::from(u.clone()))
                                .collect(),
                        ),
                    ),
                ]),
                None,
                row.occurrences_count as i64,
                None,
            );
        }
    }
}

/// `check_discovery_path` (graph path): `DEEP_DISCOVERY_PATH` past
/// `thresholds.crawl_depth_max` (default 4) from the depth-0 seed.
pub fn check_discovery_path(ctx: &mut AuditContext) {
    if !ctx.graph_access {
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "no all_inlinks export (needed for the complete internal edge list)",
        );
        return;
    }
    let Some(con) = ctx.scan.take() else {
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "no all_inlinks export (needed for the complete internal edge list)",
        );
        return;
    };
    let Some(seed_page) = ctx
        .pages
        .iter()
        .find(|p| rec(p).get("crawl_depth").and_then(Value::as_i64) == Some(0))
        .map(|p| p.url.clone())
    else {
        ctx.scan = Some(con);
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "no page at Crawl Depth 0 to use as the seed",
        );
        return;
    };
    let seed = norm(&seed_page);
    let max_depth = ctx.thresholds.crawl_depth_max;
    let host = site_host(ctx);
    let page_keys: Vec<(String, String)> = ctx
        .indexable_html_pages()
        .iter()
        .map(|&i| (norm(&ctx.pages[i].url), ctx.pages[i].url.clone()))
        .collect();
    let measured = {
        let created = AnalysisGraph::new(&con, |u: &str| norm(u), &host);
        created.ok().and_then(|mut graph| {
            graph.begin_paths(&seed).ok().flatten().map(|session| {
                let mut fired: Vec<(String, i64, Option<Vec<String>>)> = Vec::new();
                for (key, url) in &page_keys {
                    let depth = session.depth_for(key).ok().flatten();
                    if depth.is_none_or(|d| d <= max_depth) {
                        continue;
                    }
                    let depth = depth.unwrap_or(0);
                    let route = if depth < MAX_PATH_URLS {
                        session.path_to(key).ok().flatten()
                    } else {
                        None
                    };
                    fired.push((url.clone(), depth, route));
                }
                fired
            })
        })
    };
    ctx.scan = Some(con);
    let Some(fired) = measured else {
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "all_inlinks export has no internal hyperlinks",
        );
        return;
    };
    let by_norm: std::collections::HashMap<String, String> = ctx
        .pages
        .iter()
        .map(|p| (norm(&p.url), p.url.clone()))
        .collect();
    for (url, depth, route) in fired {
        let mut d = Map::new();
        d.insert("hops".into(), Value::from(depth));
        match route {
            Some(route) => {
                d.insert(
                    "path".into(),
                    Value::Array(
                        route
                            .iter()
                            .map(|k| {
                                Value::from(by_norm.get(k).cloned().unwrap_or_else(|| k.clone()))
                            })
                            .collect(),
                    ),
                );
            }
            None => {
                d.insert(
                    "path_start".into(),
                    Value::Array(vec![Value::from(
                        by_norm.get(&seed).cloned().unwrap_or_else(|| seed.clone()),
                    )]),
                );
                d.insert(
                    "path_end".into(),
                    Value::Array(vec![Value::from(url.clone())]),
                );
                d.insert("path_truncated".into(), Value::from(true));
            }
        }
        ctx.add("DEEP_DISCOVERY_PATH", Some(&url), d);
    }
}

/// `check_internal_link_graph` (graph path): `ctx.internal_linking`,
/// `DUPLICATE_INTERNAL_LINK`, `DEEP_CLICK_DEPTH`.
pub fn check_internal_link_graph(ctx: &mut AuditContext) {
    // `floor = int(ctx.thresholds.get("click_depth_max", DEFAULT_CLICK_DEPTH_FLOOR))`
    let floor = ctx
        .thresholds
        .get("click_depth_max")
        .map_or(CLICK_DEPTH_FLOOR, |v| v as i64);
    if !ctx.graph_access {
        let reason = "no all_inlinks export (needed for the complete internal edge list)";
        for check in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
            ctx.skip(check, reason);
        }
        ctx.internal_linking = unmeasured(reason);
        return;
    }
    let Some(con) = ctx.scan.take() else {
        let reason = "no all_inlinks export (needed for the complete internal edge list)";
        for check in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
            ctx.skip(check, reason);
        }
        ctx.internal_linking = unmeasured(reason);
        return;
    };
    let host = site_host(ctx);
    let measured = (|| {
        let mut graph = AnalysisGraph::new(&con, |u: &str| norm(u), &host).ok()?;
        if !graph.has_internal_hyperlinks().unwrap_or(false) {
            return None;
        }
        let positions = summarize_positions(&graph.position_totals().unwrap_or_default());
        let groups = graph
            .iter_duplicate_links(MAX_DUPLICATE_REPEATS)
            .unwrap_or_default();
        Some((positions, groups))
    })();
    ctx.scan = Some(con);
    let Some((positions, groups)) = measured else {
        let reason = "all_inlinks export has no internal hyperlinks";
        for check in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
            ctx.skip(check, reason);
        }
        ctx.internal_linking = unmeasured(reason);
        return;
    };

    // `_emit_duplicate_links`: one site-wide finding per repeated-link pattern.
    let by_norm: std::collections::HashMap<String, String> = ctx
        .pages
        .iter()
        .map(|p| (norm(&p.url), p.url.clone()))
        .collect();
    let mut buckets: Vec<DupBucket> = Vec::new();
    let mut surplus_total = 0i64;
    for group in groups {
        surplus_total += group.surplus_total as i64;
        let pattern: Vec<Map<String, Value>> = group.repeats.clone();
        let url = by_norm
            .get(&norm(&group.source_url))
            .cloned()
            .unwrap_or(group.source_url);
        match buckets.iter_mut().find(|(p, _, _)| *p == pattern) {
            Some((_, surplus, urls)) => {
                *surplus += group.surplus_total as i64;
                urls.push(url);
            }
            None => buckets.push((pattern, group.surplus_total as i64, vec![url])),
        }
    }
    for (pattern, surplus, urls) in buckets {
        let mut d = Map::new();
        d.insert("surplus_links".into(), Value::from(surplus));
        d.insert(
            "repeats".into(),
            Value::Array(pattern.into_iter().map(Value::Object).collect()),
        );
        ctx.add_site_wide_occurrences("DUPLICATE_INTERNAL_LINK", &urls, d, Some(surplus));
    }

    // `_measure_click_depth` (graph path): BFS from the crawl start URL.
    let (seed, reason) = click_depth_seed(ctx);
    let mut summary = Map::new();
    summary.insert("measured".into(), Value::from(true));
    for (k, v) in positions.clone() {
        summary.insert(k, v);
    }
    summary.insert("duplicate_edges".into(), Value::from(surplus_total));
    let edges_total = positions
        .get("edges_total")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    summary.insert(
        "duplicate_fraction".into(),
        Value::from(if edges_total > 0 {
            (surplus_total as f64 / edges_total as f64 * 10000.0).round() / 10000.0
        } else {
            0.0
        }),
    );
    let indexable_keys: Vec<(String, String)> = ctx
        .indexable_html_pages()
        .iter()
        .map(|&i| (norm(&ctx.pages[i].url), ctx.pages[i].url.clone()))
        .collect();
    let page_keys: Vec<String> = ctx
        .html_pages()
        .iter()
        .map(|&i| norm(&ctx.pages[i].url))
        .collect();
    let click_depth = match seed {
        None => {
            ctx.skip("DEEP_CLICK_DEPTH", &reason);
            Value::Object(unmeasured(&reason))
        }
        Some(seed) => {
            let con = ctx.scan.take().expect("scan restored above");
            let seed_label = by_norm.get(&seed).cloned().unwrap_or_else(|| seed.clone());
            let walked = (|| {
                let mut graph = AnalysisGraph::new(&con, |u: &str| norm(u), &host).ok()?;
                let session = graph.begin_paths(&seed).ok().flatten()?;
                let mut fired: Vec<(String, i64, Option<Vec<String>>)> = Vec::new();
                for (key, url) in &indexable_keys {
                    let depth = session.depth_for(key).ok().flatten();
                    if depth.is_none_or(|d| d <= floor) {
                        continue;
                    }
                    let depth = depth.unwrap_or(0);
                    let route = session.path_to(key).ok().flatten();
                    fired.push((url.clone(), depth, route));
                }
                let depth_closure = |key: &str| session.depth_for(key).ok().flatten();
                let result = summarize_depth(&depth_closure, &page_keys, &seed_label, floor);
                Some((fired, result))
            })();
            ctx.scan = Some(con);
            match walked {
                None => {
                    let reason = "all_inlinks export has no internal hyperlinks";
                    ctx.skip("DEEP_CLICK_DEPTH", reason);
                    Value::Object(unmeasured(reason))
                }
                Some((fired, result)) => {
                    for (url, depth, route) in fired {
                        let mut d = Map::new();
                        d.insert("click_depth".into(), Value::from(depth));
                        d.insert("floor_used".into(), Value::from(floor));
                        d.insert(
                            "threshold".into(),
                            Value::from("thresholds.click_depth_max"),
                        );
                        match route {
                            Some(route) if route.len() <= MAX_PATH_URLS as usize => {
                                d.insert(
                                    "path".into(),
                                    Value::Array(
                                        route
                                            .iter()
                                            .map(|k| {
                                                Value::from(
                                                    by_norm
                                                        .get(k)
                                                        .cloned()
                                                        .unwrap_or_else(|| k.clone()),
                                                )
                                            })
                                            .collect(),
                                    ),
                                );
                            }
                            Some(route) => {
                                let urls: Vec<Value> = route
                                    .iter()
                                    .map(|k| {
                                        Value::from(
                                            by_norm.get(k).cloned().unwrap_or_else(|| k.clone()),
                                        )
                                    })
                                    .collect();
                                d.insert(
                                    "path_start".into(),
                                    Value::Array(urls[..PATH_END_URLS].to_vec()),
                                );
                                d.insert(
                                    "path_end".into(),
                                    Value::Array(urls[urls.len() - PATH_END_URLS..].to_vec()),
                                );
                                d.insert("path_truncated".into(), Value::from(true));
                            }
                            None if depth >= MAX_PATH_URLS => {
                                d.insert(
                                    "path_start".into(),
                                    Value::Array(vec![Value::from(
                                        by_norm.get(&seed).cloned().unwrap_or_else(|| seed.clone()),
                                    )]),
                                );
                                d.insert(
                                    "path_end".into(),
                                    Value::Array(vec![Value::from(url.clone())]),
                                );
                                d.insert("path_truncated".into(), Value::from(true));
                            }
                            None => {}
                        }
                        ctx.add("DEEP_CLICK_DEPTH", Some(&url), d);
                    }
                    Value::Object(result)
                }
            }
        }
    };
    summary.insert("click_depth".into(), click_depth);
    ctx.internal_linking = summary;
}

/// `_GENERIC_ANCHORS` (the reference suite): localized non-descriptive
/// anchor phrases, Russian and English.
const GENERIC_ANCHORS: &[&str] = &[
    "тут",
    "здесь",
    "далее",
    "подробнее",
    "сюда",
    "ссылка",
    "читать далее",
    "читать дальше",
    "читайте далее",
    "читайте дальше",
    "по ссылке",
    "перейти по ссылке",
    "перейдите по ссылке",
    "нажмите здесь",
    "узнать больше",
    "больше",
    "смотрите тут",
    "here",
    "click here",
    "read more",
    "learn more",
    "more",
    "see more",
    "view more",
    "click",
    "link",
    "this",
    "this link",
    "continue reading",
    "check it out",
    "details",
];

/// `_GENERIC_ANCHOR_TRIM` (the reference suite).
const GENERIC_ANCHOR_TRIM: &str = " \t\u{a0}.,;:!?«»\"'()[]{}…—–-";

/// `_norm_anchor` (the reference suite): lowercase, collapse whitespace
/// runs to one space, strip, then strip the punctuation set and strip again.
fn norm_anchor(anchor: &str) -> String {
    let lower = anchor.to_lowercase();
    let mut cleaned = String::with_capacity(lower.len());
    let mut pending_ws = false;
    for c in lower.chars() {
        if c.is_whitespace() {
            pending_ws = true;
        } else {
            if pending_ws && !cleaned.is_empty() {
                cleaned.push(' ');
            }
            cleaned.push(c);
            pending_ws = false;
        }
    }
    cleaned
        .trim_matches(|c| GENERIC_ANCHOR_TRIM.contains(c))
        .trim()
        .to_owned()
}

/// `stored_graph_available` (the reference suite): graph checks get
/// a backend only when the stored scan is not a list scan and its `links`
/// table holds at least one row.
pub(crate) fn stored_graph_available(con: &rusqlite::Connection) -> bool {
    let list_scan = con
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some();
    !list_scan
        && con
            .query_row("SELECT 1 FROM links LIMIT 1", [], |r| r.get::<_, i64>(0))
            .optional()
            .ok()
            .flatten()
            .is_some()
}

/// `check_anchor_text` (the reference suite, graph path): one
/// `GENERIC_ANCHOR_TEXT` finding per source page carrying generic anchors.
pub fn check_anchor_text(ctx: &mut AuditContext) {
    let max_locs = ctx
        .config
        .get("output")
        .and_then(|o| o.get("max_locations_per_issue"))
        .and_then(Value::as_i64)
        .unwrap_or(200);
    // `export_keys` is always empty on a stored scan: no `*:Inlinks` frame is
    // retained, so only the graph path below can produce findings. Without a
    // stored links inventory `_graph_access` is unset and the check skips.
    if !ctx.graph_access {
        ctx.skip(
            "GENERIC_ANCHOR_TEXT",
            "no *:Inlinks export available (export 'All Inlinks' or any *:Inlinks report)",
        );
        return;
    }
    let Some(con) = ctx.scan.take() else {
        ctx.skip(
            "GENERIC_ANCHOR_TEXT",
            "no *:Inlinks export available (export 'All Inlinks' or any *:Inlinks report)",
        );
        return;
    };
    let host = site_host(ctx);
    let groups = {
        let created = AnalysisGraph::new(&con, |u: &str| norm(u), &host);
        created.ok().map(|mut graph| {
            graph
                .iter_anchor_groups(
                    |a| GENERIC_ANCHORS.contains(&norm_anchor(a).as_str()),
                    max_locs,
                )
                .unwrap_or_default()
        })
    };
    ctx.scan = Some(con);
    for group in groups.unwrap_or_default() {
        let mut evidence = Map::new();
        evidence.insert("exports".into(), json!(["all_inlinks"]));
        evidence.insert("files".into(), json!([Value::Null]));
        ctx.add_evidence(
            "GENERIC_ANCHOR_TEXT",
            Some(&group.source_url),
            None,
            group.occurrences_count as i64,
            (
                group.locations.into_iter().map(Value::Object).collect(),
                evidence,
            ),
            details([(
                "generic_links",
                Value::Array(group.generic_links.into_iter().map(Value::Object).collect()),
            )]),
        );
    }
}

/// All Inlinks `Type`
/// values that are page links, not subresources.
const NON_RESOURCE_LINK_TYPES: [&str; 6] = [
    "hyperlink",
    "canonical",
    "rel next",
    "rel prev",
    "meta refresh",
    "amphtml",
];

/// Insecure subresources: an HTTPS page
/// loading a subresource over HTTP. Reads the non-hyperlink rows of
/// `all_inlinks`; a `security_mixed` export answers the same question and is
/// preferred when present, so this skips rather than double-reports. The
/// stored edge projection keeps no resource types, so the graph route always
/// skips by name rather than pass as clean.
pub fn check_insecure_subresources(ctx: &mut AuditContext) {
    if ctx.has_export("security_mixed") {
        ctx.skip(
            "INSECURE_SUBRESOURCE",
            "native Security:Mixed Content export already covers this",
        );
        return;
    }
    let Some(records) = super::pagination::all_inlink_records(ctx) else {
        if ctx.graph_access {
            // `_graph_access` exists yet `graph.has_resource_type` is never
            // true for the stored projection (`iter_resources` yields
            // nothing).
            ctx.skip(
                "INSECURE_SUBRESOURCE",
                "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
            );
        } else {
            ctx.skip(
                "INSECURE_SUBRESOURCE",
                "no all_inlinks export (needed for the resource inventory)",
            );
        }
        return;
    };
    if !records.iter().any(|r| super::py_truthy(r.get("type"))) {
        ctx.skip(
            "INSECURE_SUBRESOURCE",
            "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
        );
        return;
    }
    let mut by_source: indexmap::IndexMap<String, Vec<String>> = indexmap::IndexMap::new();
    for rec in records {
        let (Some(source), Some(dest)) = (
            rec.get("source_url").and_then(Value::as_str),
            rec.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        if source.is_empty() || dest.is_empty() || !source.to_lowercase().starts_with("https://") {
            continue;
        }
        let link_type = rec
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if NON_RESOURCE_LINK_TYPES.contains(&link_type.as_str()) {
            continue;
        }
        if dest.to_lowercase().starts_with("http://") {
            by_source
                .entry(source.to_owned())
                .or_default()
                .push(dest.to_owned());
        }
    }
    for (source, resources) in by_source {
        let mut listed: Vec<String> = resources
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect();
        listed.sort();
        listed.truncate(20);
        ctx.add_with(
            "INSECURE_SUBRESOURCE",
            Some(&source),
            details([("resources", json!(listed))]),
            None,
            resources.len() as i64,
            None,
        );
    }
}
