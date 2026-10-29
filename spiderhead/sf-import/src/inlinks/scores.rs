//! Scoring layer: link score, inlink composition, and discovery paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_core::graph::{InlinkCompositionRow, LinkScoreConstants, PathSession};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::crawl_path::{bfs_tree_from_seed, route_from_parents};
use crate::inlinks::shared::{
    all_inlink_records, graph_access, host_of, internal_hyperlink_edges, median, rec, site_host,
    urldefrag, MAX_PATH_URLS,
};
use crate::link_score::{
    compute_link_scores, DEFAULT_DAMPING, DEFAULT_MAX_ITERATIONS, DEFAULT_TOLERANCE,
};
use crate::normalize::{is_true, norm_url, to_int, Object};

/// reference `round(x, n)` for a small fixed n: the nearest representable value
/// of the n-decimal result.
fn round_at(x: f64, decimals: usize) -> f64 {
    format!("{:.*}", decimals, x).parse().unwrap_or(x)
}

/// A JSON number for a finite float, `null` otherwise.
fn num(x: f64) -> Value {
    serde_json::Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// `_emit_link_scores`: apply the one reviewed LOW_LINK_SCORE threshold and
/// emission shape. Every page first records its computed score; the finding
/// itself only fires when the site has a meaningful score distribution to
/// compare against.
fn emit_link_scores(
    ctx: &mut AuditContext,
    count: usize,
    site_median: f64,
    score_for: &dyn Fn(&str) -> Option<f64>,
) -> Result<(), ContextError> {
    for mut page in ctx.pages_iter() {
        let norm = match norm_url(Some(&page.url())) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if let Some(score) = score_for(&norm) {
            let value = num(round_at(score, 6));
            page.mutate(|p| {
                p.metrics
                    .insert("link_score_computed".into(), value.clone());
            });
        }
    }
    if count < 5 || site_median <= 0.0 {
        return Ok(());
    }
    let ratio = ctx
        .thresholds
        .get("link_score_low_ratio")
        .and_then(Value::as_f64)
        .unwrap_or(0.25);
    for page in ctx.indexable_html_pages() {
        if rec(&page).get("crawl_depth").and_then(Value::as_i64) == Some(0) {
            continue;
        }
        let norm = match norm_url(Some(&page.url())) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let Some(score) = score_for(&norm) else {
            continue;
        };
        if score >= site_median * ratio {
            continue;
        }
        let mut details = Map::new();
        details.insert("link_score".into(), num(round_at(score, 6)));
        details.insert("site_median".into(), num(round_at(site_median, 6)));
        details.insert(
            "ratio_to_median".into(),
            num(round_at(score / site_median, 3)),
        );
        ctx.add(
            "LOW_LINK_SCORE",
            AddOpts {
                target_url: Some(page.url()),
                details,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `check_link_score`: LOW_LINK_SCORE — an iterative internal-PageRank pass.
/// One new edge changes every page's score, so this can only run once the
/// crawl's own edge list is complete: it reads the same `all_inlinks` export
/// the other inlink checks share, restricted to internal followed hyperlink
/// edges, and flags indexable pages scoring far below the site's own median —
/// a signal a raw inlink *count* misses, since a page can hold several inlinks
/// and still be starved of link equity when every one is nofollow or itself
/// comes from a poorly linked page.
pub fn check_link_score(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let records = all_inlink_records(ctx);
    let Some(records) = records else {
        let stats = ctx.graph_access.as_deref().and_then(|g| {
            g.link_score(LinkScoreConstants {
                damping: DEFAULT_DAMPING,
                max_iterations: DEFAULT_MAX_ITERATIONS as u64,
                tolerance: DEFAULT_TOLERANCE,
            })
        });
        let Some(stats) = stats else {
            ctx.skip(
                "LOW_LINK_SCORE",
                "all_inlinks export has no internal followed hyperlinks",
            );
            return Ok(());
        };
        let count = stats.count() as usize;
        let site_median = stats.median();
        let score_for = move |key: &str| stats.score_for(key);
        return emit_link_scores(ctx, count, site_median, &score_for);
    };
    let host = site_host(ctx);
    let edges = internal_hyperlink_edges(&records, &host)?;
    if edges.is_empty() {
        ctx.skip(
            "LOW_LINK_SCORE",
            "all_inlinks export has no internal followed hyperlinks",
        );
        return Ok(());
    }
    let mut urls: BTreeSet<String> = BTreeSet::new();
    for page in ctx.pages_iter() {
        urls.insert(norm_url(Some(&page.url()))?);
    }
    let scores = compute_link_scores(
        &edges,
        Some(&urls),
        DEFAULT_DAMPING,
        DEFAULT_MAX_ITERATIONS,
        DEFAULT_TOLERANCE,
    );
    let site_median = median(&scores.values().copied().collect::<Vec<f64>>());
    let count = scores.len();
    let score_for = move |key: &str| scores.get(key).copied();
    emit_link_scores(ctx, count, site_median, &score_for)
}

/// `check_inlink_composition`: ONLY_NOFOLLOW_INLINKS /
/// ONLY_NONINDEXABLE_SOURCE_INLINKS. Both are aggregates over *every* inlink
/// a URL has: "only nofollow" is falsified the moment one more, follow inlink
/// turns up, and the same is true of "only from non-indexable sources" — so
/// neither is provable before the crawl (and the complete `all_inlinks`
/// export) is finished.
pub fn check_inlink_composition(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let records = all_inlink_records(ctx);
    let Some(records) = &records else {
        // The graph borrow must not outlive the page pass below, so the
        // presence check runs first without holding it, then the index of
        // every page's indexability is built (the page view needs &mut ctx),
        // and only then the row iterator borrows the graph again.
        if ctx.graph_access.is_none() {
            for check_id in ["ONLY_NOFOLLOW_INLINKS", "ONLY_NONINDEXABLE_SOURCE_INLINKS"] {
                ctx.skip(
                    check_id,
                    "no all_inlinks export (needed for the complete inlink list)",
                );
            }
            return Ok(());
        }
        let mut index: HashMap<String, bool> = HashMap::new();
        for page in ctx.pages_iter() {
            index.insert(norm_url(Some(&page.url()))?, page.map(|p| p.is_indexable()));
        }
        let Some(graph) = graph_access(ctx) else {
            return Ok(()); // presence checked above
        };
        let is_indexable = move |source: &str| -> Option<bool> {
            let key = norm_url(Some(source)).unwrap_or_default();
            index.get(&key).copied()
        };
        let rows: Vec<InlinkCompositionRow> =
            graph.iter_inlink_composition(&is_indexable, 20).collect();
        return emit_inlink_composition(ctx, rows);
    };

    let host = site_host(ctx);
    // Group by the fragment-free page identity, not the raw destination: a
    // crawled page is one row keyed by norm_url(page.url), which never carries
    // a fragment, while norm_url(dest) still does — so "/target" and
    // "/target#details" grouped by raw destination formed two composition
    // buckets, and the fragment-bearing one could never resolve to the crawled
    // page, silently dropping its nofollow/nonindexable-source evidence. Each
    // link keeps its own raw destination_url below; only the grouping key is
    // defragmented.
    let mut by_dest: IndexMap<String, Vec<Object>> = IndexMap::new();
    for item in records {
        let (Some(source), Some(dest)) = (
            item.get("source_url").and_then(Value::as_str),
            item.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        if source.is_empty() || dest.is_empty() {
            continue;
        }
        if let Some(t) = item.get("type") {
            let is_hyperlink = t
                .as_str()
                .map(|s| s.trim().to_lowercase() == "hyperlink")
                .unwrap_or(false);
            if !is_hyperlink {
                continue;
            }
        }
        let dh = host_of(dest);
        if !dh.is_empty() && dh != host {
            continue; // external destination — not this page's own composition
        }
        let dest_key = urldefrag(&norm_url(Some(dest))?).to_owned();
        by_dest.entry(dest_key).or_default().push(item.clone());
    }

    let mut rows: Vec<InlinkCompositionRow> = Vec::new();
    for (dest_key, links) in &by_dest {
        let follows: Vec<bool> = links
            .iter()
            .map(|link| match link.get("follow") {
                None | Some(Value::Null) => true,
                Some(v) => is_true(v),
            })
            .collect();
        let mut source_indexability: Vec<bool> = Vec::new();
        for link in links {
            let source = link.get("source_url").and_then(Value::as_str).unwrap_or("");
            if let Some(page) = ctx.page_by_norm(&norm_url(Some(source))?) {
                source_indexability.push(page.map(|p| p.is_indexable()));
            }
        }
        let source_examples: Vec<String> = links
            .iter()
            .map(|link| link.get("source_url").and_then(Value::as_str).unwrap_or(""))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .take(20)
            .collect();
        rows.push(InlinkCompositionRow {
            destination_key: dest_key.clone(),
            occurrences_count: links.len() as u64,
            all_nofollow: !follows.iter().any(|f| *f),
            has_known_source: !source_indexability.is_empty(),
            has_indexable_source: source_indexability.iter().any(|f| *f),
            source_examples,
        });
    }
    emit_inlink_composition(ctx, rows)
}

/// `sorted(sources)[:20]`.
fn sorted_capped(sources: &[String]) -> Vec<Value> {
    let mut sorted: Vec<&str> = sources.iter().map(String::as_str).collect();
    sorted.sort();
    sorted.into_iter().take(20).map(Value::from).collect()
}

/// The shared detail block of both composition findings.
fn composition_details(row: &InlinkCompositionRow) -> Map<String, Value> {
    json!({
        "inlink_count": row.occurrences_count,
        "sources": sorted_capped(&row.source_examples),
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

/// `_emit_inlink_composition`: apply the shared target eligibility and the two
/// issue payloads.
fn emit_inlink_composition(
    ctx: &mut AuditContext,
    rows: Vec<InlinkCompositionRow>,
) -> Result<(), ContextError> {
    for row in rows {
        // is_indexable is a property of the live page, so the 2xx-preferring
        // representative answers "is this destination's inlink composition
        // worth flagging". The key is already norm_url() with the fragment
        // stripped, and the page index's keys are norm_url(page.url) for URLs
        // that never carry a fragment, so it is the correct lookup key as-is.
        let target = ctx.page_by_norm(&row.destination_key);
        let Some(target) = target else {
            continue;
        };
        if !target.map(|p| p.is_indexable()) {
            continue;
        }
        if rec(&target).get("crawl_depth").and_then(Value::as_i64) == Some(0) {
            continue; // the homepage's inlink composition is not the whole story
        }
        if row.all_nofollow {
            ctx.add(
                "ONLY_NOFOLLOW_INLINKS",
                AddOpts {
                    target_url: Some(target.url()),
                    occurrences_count: Some(row.occurrences_count as i64),
                    details: composition_details(&row),
                    ..Default::default()
                },
            )?;
        }
        if row.has_known_source && !row.has_indexable_source {
            ctx.add(
                "ONLY_NONINDEXABLE_SOURCE_INLINKS",
                AddOpts {
                    target_url: Some(target.url()),
                    occurrences_count: Some(row.occurrences_count as i64),
                    details: composition_details(&row),
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}

/// `_path_endpoint`: the crawled spelling of a route member, or the key.
pub(crate) fn path_endpoint(ctx: &mut AuditContext, key: &str) -> String {
    ctx.page_by_norm(key)
        .map(|p| p.url())
        .unwrap_or_else(|| key.to_owned())
}

/// `_emit_discovery_paths`: apply the one reviewed DEEP_DISCOVERY_PATH
/// threshold and issue shape.
fn emit_discovery_paths(
    ctx: &mut AuditContext,
    path_for: &dyn Fn(&str) -> Option<Vec<String>>,
    depth_for: &dyn Fn(&str) -> Option<i64>,
    seed: &str,
) -> Result<(), ContextError> {
    let max_depth = ctx
        .thresholds
        .get("crawl_depth_max")
        .and_then(to_int)
        .unwrap_or(4);
    for page in ctx.indexable_html_pages() {
        let url = page.url();
        let key = norm_url(Some(&url))?;
        let Some(depth) = depth_for(&key) else {
            continue;
        };
        if depth <= max_depth {
            continue;
        }
        // No emptiness filter on the route: an empty `Some` is still a measured
        // route and must render as `path: []`, not as a truncation.
        let path_norm = if depth < MAX_PATH_URLS as i64 {
            path_for(&key)
        } else {
            None
        };
        let mut details = Map::new();
        details.insert("hops".into(), Value::from(depth));
        match path_norm {
            Some(path_norm) => {
                let path: Vec<Value> = path_norm
                    .iter()
                    .map(|item| Value::from(path_endpoint(ctx, item)))
                    .collect();
                details.insert("path".into(), Value::Array(path));
            }
            None => {
                details.insert(
                    "path_start".into(),
                    Value::Array(vec![Value::from(path_endpoint(ctx, seed))]),
                );
                details.insert("path_end".into(), Value::Array(vec![Value::from(url)]));
                details.insert("path_truncated".into(), Value::from(true));
            }
        }
        ctx.add(
            "DEEP_DISCOVERY_PATH",
            AddOpts {
                target_url: Some(page.url()),
                details,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// The first crawled page at Crawl Depth 0, when any page carries it.
fn depth_zero_seed(ctx: &mut AuditContext) -> Option<String> {
    ctx.pages_iter().into_iter().find_map(|page| {
        let is_root = rec(&page).get("crawl_depth").and_then(Value::as_i64) == Some(0);
        is_root.then(|| page.url())
    })
}

/// `check_discovery_path`: DEEP_DISCOVERY_PATH — the concrete shortest click
/// path from the seed. The Crawl Depth column already reports each page's
/// distance from the start URL, but not the route that produces it, and only a
/// breadth-first walk over the *finished* internal hyperlink graph can
/// reconstruct one. Flags indexable pages whose shortest route exceeds the
/// same `crawl_depth_max` threshold DEEP_CRAWL_DEPTH already uses, this time
/// with the actual path attached.
pub fn check_discovery_path(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let records = all_inlink_records(ctx);
    let Some(records) = records else {
        match graph_access(ctx) {
            None => {
                ctx.skip(
                    "DEEP_DISCOVERY_PATH",
                    "no all_inlinks export (needed for the complete internal edge list)",
                );
                return Ok(());
            }
            Some(g) if !g.has_internal_hyperlinks() => {
                ctx.skip(
                    "DEEP_DISCOVERY_PATH",
                    "all_inlinks export has no internal hyperlinks",
                );
                return Ok(());
            }
            Some(_) => {}
        }
        // This seed is deliberately NOT the click-depth seed: discovery paths
        // answer "how does a crawler that starts anywhere reach this page", so
        // the first page the crawl itself placed at depth 0 is the walk root.
        let Some(seed_url) = depth_zero_seed(ctx) else {
            ctx.skip(
                "DEEP_DISCOVERY_PATH",
                "no page at Crawl Depth 0 to use as the seed",
            );
            return Ok(());
        };
        let seed_norm = norm_url(Some(&seed_url))?;
        let session = ctx
            .graph_access
            .as_deref()
            .and_then(|g| g.begin_paths(&seed_norm));
        let Some(session) = session else {
            ctx.skip(
                "DEEP_DISCOVERY_PATH",
                "all_inlinks export has no internal hyperlinks",
            );
            return Ok(());
        };
        // One session, two closures: share it behind an Rc.
        let shared: Rc<dyn PathSession> = Rc::from(session);
        let shared_depths = Rc::clone(&shared);
        let path_for = move |key: &str| shared.path_to(key);
        let depth_for = move |key: &str| shared_depths.depth_for(key).map(|d| d as i64);
        return emit_discovery_paths(ctx, &path_for, &depth_for, &seed_norm);
    };

    let host = site_host(ctx);
    let edges = internal_hyperlink_edges(&records, &host)?;
    if edges.is_empty() {
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "all_inlinks export has no internal hyperlinks",
        );
        return Ok(());
    }
    let Some(seed_url) = depth_zero_seed(ctx) else {
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "no page at Crawl Depth 0 to use as the seed",
        );
        return Ok(());
    };
    let seed_norm = norm_url(Some(&seed_url))?;
    let (depths_raw, parents_raw) = bfs_tree_from_seed(&edges, &seed_norm);
    // Native evidence and the SF-shaped record bridge can disagree only in
    // harmless URL spelling (notably a trailing slash). The page view is keyed
    // by norm_url, so normalize both sides of the predecessor tree once before
    // emission; without this every lookup misses and every finding falls back
    // to the bounded route sample.
    let mut depths: BTreeMap<String, i64> = BTreeMap::new();
    for (key, depth) in depths_raw {
        depths.insert(norm_url(Some(&key))?, depth as i64);
    }
    let mut parents: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (key, parent) in parents_raw {
        let norm_parent = match parent {
            Some(p) => Some(norm_url(Some(&p))?),
            None => None,
        };
        parents.insert(norm_url(Some(&key))?, norm_parent);
    }
    let path_for = move |key: &str| route_from_parents(&parents, key);
    let depth_for = move |key: &str| depths.get(key).copied();
    emit_discovery_paths(ctx, &path_for, &depth_for, &seed_norm)
}
