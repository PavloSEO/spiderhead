//! Graph layer: click depth, duplicate-link and internal-link-graph checks.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::internal_linking::{summarize_depth, summarize_positions, unmeasured};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_core::graph::{DuplicateLinkGroup, GraphAccess, PathSession};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::crawl_path::{bfs_tree_from_seed, route_from_parents};
use crate::inlinks::scores::path_endpoint;
use crate::inlinks::shared::{
    all_inlink_records, internal_hyperlink_edges, internal_hyperlink_records, rec, site_host,
    MAX_PATH_URLS,
};
use crate::normalize::{norm_url, to_int, Object};

/// One repeated-link pattern: (destination, anchor, count) per repeat.
type RepeatPattern = Vec<(String, String, i64)>;

/// Pages sharing one repeat pattern: (surplus total, page URLs).
type DuplicateBucket = (i64, Vec<String>);

/// `(session, seed, reason)` of `open_click_depth_session`: `session` is
/// `None`, with `reason` set, when there is nothing to walk.
pub type ClickDepthSession = (Option<Box<dyn PathSession>>, Option<String>, String);

/// Depth lookup of a click-depth walk.
type DepthFn = Box<dyn Fn(&str) -> Option<i64>>;

/// Route lookup of a click-depth walk.
type RouteFn = Box<dyn Fn(&str) -> Option<Vec<String>>>;

/// `_MAX_DUPLICATE_REPEATS`.
const MAX_DUPLICATE_REPEATS: usize = 20;

/// The default click-depth floor, used when no config supplies one. Ten, not
/// the four `crawl_depth_max` uses for DEEP_CRAWL_DEPTH: that threshold answers
/// "is this deeper than the site's own depth budget", and a site with a long,
/// healthy paginated archive trips it everywhere. This one answers the
/// different, coarser question the depth histogram exists for — which pages are
/// so far from the start URL that navigation does not reach them at all.
pub(crate) const DEFAULT_CLICK_DEPTH_FLOOR: i64 = 10;

/// `_PATH_END_URLS`.
const PATH_END_URLS: usize = 5;

/// `click_depth_seed`: the URL the click-depth walk starts from, or the reason
/// there is none. The crawl's own recorded start URL is the only trustworthy
/// seed; Crawl Depth 0 is the fallback for an SF export, which records no start
/// URL of its own — and it is only usable when exactly one page carries it. A
/// sitemap-seeded native crawl records 0 for every seeded URL, so picking "a
/// page at depth 0" would silently walk from an arbitrary article and report a
/// depth histogram of the wrong site.
pub(crate) fn click_depth_seed(
    ctx: &mut AuditContext,
) -> Result<(Option<String>, String), ContextError> {
    if let Some(start) = ctx
        .start_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    {
        return Ok((Some(norm_url(Some(&start))?), String::new()));
    }
    let mut roots: Vec<String> = Vec::new();
    for page in ctx.pages_iter() {
        let is_root = rec(&page).get("crawl_depth").and_then(Value::as_i64) == Some(0);
        if is_root {
            roots.push(page.url());
        }
    }
    match roots.len() {
        0 => Ok((
            None,
            "the run recorded no crawl start URL and no page is at Crawl Depth 0, \
             so there is no page to walk the link graph from"
                .to_owned(),
        )),
        1 => Ok((Some(norm_url(Some(&roots[0]))?), String::new())),
        n => Ok((
            None,
            format!(
                "the run recorded no crawl start URL and {n} pages carry Crawl \
                 Depth 0, so that column cannot identify where the crawl began"
            ),
        )),
    }
}

/// `_emit_duplicate_links`: one finding per repeated-link pattern — the pages
/// that write the same links twice. A navigation link written twice on every
/// page is one template defect. Pages that share the same repeats become one
/// finding with their page count, so a site-wide menu does not appear once per
/// URL. Returns the surplus.
fn emit_duplicate_links(
    ctx: &mut AuditContext,
    groups: impl Iterator<Item = DuplicateLinkGroup>,
) -> Result<i64, ContextError> {
    let mut surplus_total = 0i64;
    // pattern (destination, anchor, count) per repeat -> (surplus, urls)
    let mut buckets: IndexMap<RepeatPattern, DuplicateBucket> = IndexMap::new();
    for group in groups {
        surplus_total += group.surplus_total as i64;
        let pattern: Vec<(String, String, i64)> = group
            .repeats
            .iter()
            .map(|r| {
                (
                    r.get("destination")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    r.get("anchor")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    r.get("count").and_then(Value::as_i64).unwrap_or_default(),
                )
            })
            .collect();
        let page = ctx.page_by_norm(&norm_url(Some(&group.source_url))?);
        let entry = buckets
            .entry(pattern)
            .or_insert_with(|| (group.surplus_total as i64, Vec::new()));
        entry
            .1
            .push(page.map(|p| p.url()).unwrap_or(group.source_url));
    }
    for (pattern, (surplus, urls)) in &buckets {
        // No threshold here, and that is deliberate: "written more than once"
        // is a count, not a judgement calibrated against a number somebody
        // chose.
        let repeats: Vec<Value> = pattern
            .iter()
            .map(|(d, a, c)| json!({"destination": d, "anchor": a, "count": c}))
            .collect();
        let mut details = Map::new();
        details.insert("surplus_links".into(), Value::from(*surplus));
        details.insert("repeats".into(), Value::Array(repeats));
        ctx.add_site_wide("DUPLICATE_INTERNAL_LINK", urls, details, Some(*surplus))?;
    }
    Ok(surplus_total)
}

/// `_emit_deep_click_depth`: flag indexable pages further from the start URL
/// than the configured floor.
fn emit_deep_click_depth(
    ctx: &mut AuditContext,
    depth_for: &dyn Fn(&str) -> Option<i64>,
    floor: i64,
    path_for: &dyn Fn(&str) -> Option<Vec<String>>,
    seed: &str,
    compact_routes: bool,
) -> Result<(), ContextError> {
    for page in ctx.indexable_html_pages() {
        let url = page.url();
        let norm = norm_url(Some(&url))?;
        let Some(depth) = depth_for(&norm) else {
            continue;
        };
        if depth <= floor {
            continue;
        }
        let mut details = Map::new();
        details.insert("click_depth".into(), Value::from(depth));
        // Named in every finding: the verdict is "deeper than this number",
        // and the number is configuration, not a property of the site.
        details.insert("floor_used".into(), Value::from(floor));
        details.insert(
            "threshold".into(),
            Value::from("thresholds.click_depth_max"),
        );
        // Do not rebuild a million-node predecessor chain just to render a
        // bounded finding sample. The depth is the verdict; for a long route
        // retain its known endpoints and say the middle was omitted.
        let route = if !compact_routes || depth < MAX_PATH_URLS as i64 {
            path_for(&norm).filter(|r| !r.is_empty())
        } else {
            None
        };
        match route {
            Some(route) => {
                let urls: Vec<String> = route
                    .iter()
                    .map(|item| {
                        ctx.page_by_norm(item)
                            .map(|p| p.url())
                            .unwrap_or_else(|| item.clone())
                    })
                    .collect();
                if urls.len() <= MAX_PATH_URLS {
                    details.insert(
                        "path".into(),
                        Value::Array(urls.into_iter().map(Value::from).collect()),
                    );
                } else {
                    let starts: Vec<Value> = urls[..PATH_END_URLS]
                        .iter()
                        .map(|u| Value::from(u.clone()))
                        .collect();
                    let ends: Vec<Value> = urls[urls.len() - PATH_END_URLS..]
                        .iter()
                        .map(|u| Value::from(u.clone()))
                        .collect();
                    details.insert("path_start".into(), Value::Array(starts));
                    details.insert("path_end".into(), Value::Array(ends));
                    details.insert("path_truncated".into(), Value::from(true));
                }
            }
            None if depth >= MAX_PATH_URLS as i64 => {
                details.insert(
                    "path_start".into(),
                    Value::Array(vec![Value::from(path_endpoint(ctx, seed))]),
                );
                details.insert("path_end".into(), Value::Array(vec![Value::from(url)]));
                details.insert("path_truncated".into(), Value::from(true));
            }
            None => {}
        }
        ctx.add(
            "DEEP_CLICK_DEPTH",
            AddOpts {
                target_url: Some(page.url()),
                details,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// Store the "measured: false" block under `ctx.internal_linking`.
fn set_unmeasured(ctx: &mut AuditContext, reason: &str) {
    ctx.internal_linking = unmeasured(reason).as_object().cloned().unwrap_or_default();
}

/// `check_internal_link_graph`: DEEP_CLICK_DEPTH / DUPLICATE_INTERNAL_LINK,
/// and the graph summary both read (#634). One pass over the complete edge
/// inventory produces three things a per-edge check cannot: the distribution
/// of edges by page position, how many edges are repeats of another edge, and
/// how many clicks from the crawl's start URL each page is. The numbers land
/// in `ctx.internal_linking` for the audit summary; the two checks above are
/// the verdicts drawn from them.
///
/// DEEP_CLICK_DEPTH is not DEEP_DISCOVERY_PATH with a different name. That
/// check reads the Crawl Depth column to pick a seed and flags anything past
/// `crawl_depth_max`, the same budget DEEP_CRAWL_DEPTH uses. This one refuses
/// to start unless the run recorded where the crawl actually began (see
/// `click_depth_seed`), and its floor is the separate, coarser
/// `click_depth_max` — the depth past which navigation has stopped reaching a
/// page at all, rather than the depth past which a site's own budget is
/// exceeded.
pub fn check_internal_link_graph(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let floor = to_int(
        ctx.thresholds
            .get("click_depth_max")
            .unwrap_or(&Value::from(DEFAULT_CLICK_DEPTH_FLOOR)),
    )
    .unwrap_or(DEFAULT_CLICK_DEPTH_FLOOR);
    let records = all_inlink_records(ctx);
    let positions;
    let surplus;
    if let Some(records) = &records {
        let host = site_host(ctx);
        let internal = internal_hyperlink_records(records, &host);
        if internal.is_empty() {
            let reason = "all_inlinks export has no internal hyperlinks";
            for check_id in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
                ctx.skip(check_id, reason);
            }
            set_unmeasured(ctx, reason);
            return Ok(());
        }
        let mut totals: BTreeMap<String, i64> = BTreeMap::new();
        for item in &internal {
            let position = item
                .get("link_position")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            *totals.entry(position.to_owned()).or_default() += 1;
        }
        positions = summarize_positions(&totals);
        surplus = emit_duplicate_links(
            ctx,
            duplicate_groups_from_records(&internal, MAX_DUPLICATE_REPEATS).into_iter(),
        )?;
    } else {
        let Some(graph) = ctx.graph_access.as_deref() else {
            let reason = "no all_inlinks export (needed for the complete internal edge list)";
            for check_id in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
                ctx.skip(check_id, reason);
            }
            set_unmeasured(ctx, reason);
            return Ok(());
        };
        if !graph.has_internal_hyperlinks() {
            let reason = "all_inlinks export has no internal hyperlinks";
            for check_id in ["DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"] {
                ctx.skip(check_id, reason);
            }
            set_unmeasured(ctx, reason);
            return Ok(());
        }
        let totals: BTreeMap<String, i64> = graph
            .position_totals()
            .into_iter()
            .map(|(k, v)| (k, v as i64))
            .collect();
        positions = summarize_positions(&totals);
        // Own the groups before the mutable emission pass: the iterator holds
        // the graph borrow, the emitter needs the context.
        let groups: Vec<DuplicateLinkGroup> =
            graph.iter_duplicate_links(MAX_DUPLICATE_REPEATS).collect();
        surplus = emit_duplicate_links(ctx, groups.into_iter())?;
    }
    let edges_total = positions
        .get("edges_total")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let duplicate_fraction = if edges_total != 0 {
        round4(surplus as f64 / edges_total as f64)
    } else {
        0.0
    };
    let click_depth = measure_click_depth(ctx, records.as_deref(), floor)?;
    let mut out = Map::new();
    out.insert("measured".into(), Value::from(true));
    if let Some(fields) = positions.as_object() {
        for (k, v) in fields {
            out.insert(k.clone(), v.clone());
        }
    }
    out.insert("duplicate_edges".into(), Value::from(surplus));
    out.insert(
        "duplicate_fraction".into(),
        serde_json::Number::from_f64(duplicate_fraction)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    );
    out.insert("click_depth".into(), click_depth);
    ctx.internal_linking = out;
    Ok(())
}

/// reference `round(x, 4)`: nearest representable value of the four-decimal
/// result.
pub(crate) fn round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// `_duplicate_groups_from_records`: the export-side twin of
/// `AnalysisGraph.iter_duplicate_links`.
pub fn duplicate_groups_from_records(
    records: &[Object],
    max_repeats: usize,
) -> Vec<DuplicateLinkGroup> {
    let mut by_source: IndexMap<String, IndexMap<(String, String), i64>> = IndexMap::new();
    for item in records {
        let source = item
            .get("source_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let destination = item
            .get("destination_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let anchor = item
            .get("anchor")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        *by_source
            .entry(source)
            .or_default()
            .entry((destination, anchor))
            .or_default() += 1;
    }
    let mut groups = Vec::new();
    for (source, pairs) in by_source {
        let mut repeated: Vec<((String, String), i64)> =
            pairs.into_iter().filter(|(_, count)| *count > 1).collect();
        if repeated.is_empty() {
            continue;
        }
        // Most repeated first, ties in destination order.
        repeated.sort_by(|a, b| {
            let key_a = (-a.1, (a.0).0.as_str());
            let key_b = (-b.1, (b.0).0.as_str());
            key_a.cmp(&key_b)
        });
        let surplus_total: i64 = repeated.iter().map(|(_, count)| count - 1).sum();
        let repeats: Vec<Map<String, Value>> = repeated
            .iter()
            .take(max_repeats)
            .map(|((destination, anchor), count)| {
                json!({"destination": destination, "anchor": anchor, "count": count})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        groups.push(DuplicateLinkGroup {
            source_url: source,
            surplus_total: surplus_total.max(0) as u64,
            repeats,
        });
    }
    groups
}

/// `open_click_depth_session`: begin the followed-graph walk from the crawl's
/// start — `(session, seed, reason)`; `session` is `None`, with `reason` set,
/// when there is nothing to walk. `graph` is passed separately so callers can
/// hand in a graph that does not live in the context.
pub fn open_click_depth_session(
    ctx: &mut AuditContext,
    graph: &dyn GraphAccess,
) -> Result<ClickDepthSession, ContextError> {
    let (seed, reason) = click_depth_seed(ctx)?;
    let Some(seed) = seed else {
        return Ok((None, None, reason));
    };
    match graph.begin_paths(&seed) {
        Some(session) => Ok((Some(session), Some(seed), String::new())),
        None => Ok((
            None,
            Some(seed),
            "all_inlinks export has no internal hyperlinks".to_owned(),
        )),
    }
}

/// `_measure_click_depth`: walk the followed internal graph from the start
/// URL; emit and describe. `records` is `None` exactly when the graph backend
/// is in play (mirrors the reference `graph` argument).
fn measure_click_depth(
    ctx: &mut AuditContext,
    records: Option<&[Object]>,
    floor: i64,
) -> Result<Value, ContextError> {
    let depth_of: DepthFn;
    let route_of: RouteFn;
    let seed;
    if records.is_none() {
        let (found, reason) = click_depth_seed(ctx)?;
        let Some(found) = found else {
            ctx.skip("DEEP_CLICK_DEPTH", &reason);
            return Ok(unmeasured(&reason));
        };
        seed = found;
        // Own the session before the mutable emission pass: begin_paths hands
        // back an owned box, so the graph borrow ends here.
        let session = ctx
            .graph_access
            .as_deref()
            .and_then(|g| g.begin_paths(&seed));
        let Some(session) = session else {
            let reason = "all_inlinks export has no internal hyperlinks";
            ctx.skip("DEEP_CLICK_DEPTH", reason);
            return Ok(unmeasured(reason));
        };
        let shared: Rc<dyn PathSession> = Rc::from(session);
        let shared2 = Rc::clone(&shared);
        depth_of = Box::new(move |key: &str| shared.depth_for(key).map(|d| d as i64));
        route_of = Box::new(move |key: &str| shared2.path_to(key));
        emit_deep_click_depth(ctx, &depth_of, floor, &route_of, &seed, true)?;
    } else {
        let (found, reason) = click_depth_seed(ctx)?;
        let Some(found) = found else {
            ctx.skip("DEEP_CLICK_DEPTH", &reason);
            return Ok(unmeasured(&reason));
        };
        seed = found;
        let host = site_host(ctx);
        let edges = internal_hyperlink_edges(records.unwrap_or(&[]), &host)?;
        if edges.is_empty() {
            let reason = "all_inlinks export has no internal followed hyperlinks";
            ctx.skip("DEEP_CLICK_DEPTH", reason);
            return Ok(unmeasured(reason));
        }
        // The parent tree, never the routes themselves: one entry per node
        // instead of one per hop of every route.
        let (depths, parents) = bfs_tree_from_seed(&edges, &seed);
        depth_of = Box::new(move |key: &str| depths.get(key).copied().map(|d| d as i64));
        route_of = Box::new(move |key: &str| route_from_parents(&parents, key));
        emit_deep_click_depth(ctx, &depth_of, floor, &route_of, &seed, false)?;
    }
    let seed_display = ctx
        .page_by_norm(&seed)
        .map(|p| p.url())
        .unwrap_or_else(|| seed.clone());
    let page_keys = ctx.html_page_keys();
    Ok(summarize_depth(
        &depth_of,
        page_keys.iter().map(String::as_str),
        &seed_display,
        floor,
    ))
}
