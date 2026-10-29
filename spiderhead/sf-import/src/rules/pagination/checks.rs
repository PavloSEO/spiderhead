//! The ctx-emitting pagination checks: the rel="next" series graph
//! (`PAGINATION_LOOP`, `UNLINKED_PAGINATION_SERIES`) and the numbered
//! sequence verdicts (`PAGINATION_SEQUENCE_ERROR`, contradictions included).

use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::{norm_url, to_int};
use crate::pyvalue::{py_eq, truthy, truthy_opt};

use super::graph::{
    display_url, expected_step, pagination_loops, pagination_page_number, series_from,
    series_unjudged_reason, unjudged_series_skip_reason,
};
use super::{pagination_key, pagination_record, pagination_records};
use crate::rules::shared::{con, rec};

pub fn check_pagination_series(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let mut next_map: IndexMap<String, String> = IndexMap::new();
    let mut complete_sources = 0i64;
    for (page, rec) in pagination_records(ctx)? {
        complete_sources += 1;
        let nxt = rec.get("rel_next");
        if truthy_opt(nxt) {
            let Some(nxt) = nxt.and_then(Value::as_str) else {
                return Err(ContextError::Value(
                    "expected string for pagination declaration".to_owned(),
                ));
            };
            let from = pagination_key(ctx, &page.url())?;
            let to = pagination_key(ctx, nxt)?;
            next_map.insert(from, to);
        }
    }
    if next_map.is_empty() {
        ctx.skip("PAGINATION_LOOP", "no rel=\"next\" column in Internal:All");
        ctx.skip(
            "UNLINKED_PAGINATION_SERIES",
            "no rel=\"next\" column in Internal:All",
        );
        return Ok(());
    }

    // No hop cap here: the configured redirect_hop_cap exists to bound *redirect*
    // chains, which fan out across the whole site, and reusing it verbatim let a
    // 21-page rel="next" cycle outrun a 20-hop default and read as clean (#204).
    // series_from terminates on the graph's own structure instead.
    let has_predecessor: HashSet<String> = next_map.values().cloned().collect();

    let (loops, in_loop) = pagination_loops(&next_map);
    for (start, path, loops_to) in &loops {
        let mut series = Vec::new();
        for n in path {
            series.push(url_of(ctx, n)?);
        }
        let target_url = url_of(ctx, start)?;
        let loops_back_to = url_of(ctx, loops_to)?;
        ctx.add(
            "PAGINATION_LOOP",
            AddOpts {
                target_url: Some(target_url),
                details: {
                    let mut d = Map::new();
                    d.insert("series".into(), json!(series));
                    d.insert("loops_back_to".into(), json!(loops_back_to));
                    d
                },
                ..AddOpts::default()
            },
        )?;
    }

    if ctx.scan_con.is_some() {
        let eligible = spiderhead_store::head_links::eligible_count(con(ctx))
            .map_err(|e| ContextError::Value(e.to_string()))?;
        let mut incomplete = complete_sources < eligible;
        for destination in next_map.values() {
            let target = match ctx.page_by_url(destination) {
                Some(p) => Some(p),
                None => {
                    let norm = norm_url(Some(destination))?;
                    ctx.page_by_norm(&norm)
                }
            };
            let bad = match &target {
                None => true,
                Some(t) => {
                    !t.map(|p| p.is_2xx() && p.is_html())
                        || pagination_key(ctx, &t.url())? != *destination
                        || spiderhead_store::head_links::for_url(con(ctx), &t.url())
                            .map_err(|e| ContextError::Value(e.to_string()))?
                            .is_none()
                }
            };
            if bad {
                incomplete = true;
                break;
            }
        }
        if incomplete {
            let reason = "not measured completely by this native crawl: one or more pagination graph documents lack complete retained annotations";
            ctx.skip("PAGINATION_LOOP", reason);
            ctx.skip("UNLINKED_PAGINATION_SERIES", reason);
            return Ok(()); // known loops above remain findings; negative graph claims stay unavailable
        }
    }

    // The cheap per-page filters run before the walk, not after it, so a full
    // chain is only ever traversed for a head that can actually produce a
    // finding — a handful per crawl, against one traversal per page if the
    // order were reversed.
    for start in next_map.keys() {
        if has_predecessor.contains(start) || in_loop.contains(start) {
            continue; // not a head of its series, or already reported as a loop
        }
        let page = match ctx.page_by_url(start) {
            Some(p) => Some(p),
            None => {
                let norm = norm_url(Some(start))?;
                ctx.page_by_norm(&norm)
            }
        };
        let Some(page) = page else { continue };
        let (indexable, rec_obj) =
            page.map(|p| (p.is_indexable(), rec(p).cloned().unwrap_or_default()));
        if !indexable
            || py_eq(
                &rec_obj.get("crawl_depth").cloned().unwrap_or(Value::Null),
                &json!(0),
            )
        {
            continue;
        }
        let inlinks = rec_obj.get("inlinks").cloned().unwrap_or(Value::Null);
        if inlinks.is_null() || to_int(&inlinks).is_some_and(|n| n > 0) {
            continue;
        }
        let (path, _) = series_from(&next_map, start);
        if path.len() < 2 {
            continue;
        }
        let mut series = Vec::new();
        for n in &path {
            series.push(url_of(ctx, n)?);
        }
        ctx.add(
            "UNLINKED_PAGINATION_SERIES",
            AddOpts {
                target_url: Some(page.url()),
                details: {
                    let mut d = Map::new();
                    d.insert("series".into(), json!(series));
                    d.insert("length".into(), json!(path.len()));
                    d
                },
                ..AddOpts::default()
            },
        )?;
    }
    Ok(())
}

/// `_url_of` inside `check_pagination_series`: display only, like the
/// equivalent helper in check_canonical_chain — the walk over `next_map`
/// already decided the series and the loop before any URL is printed.
fn url_of(ctx: &mut AuditContext, n: &str) -> Result<String, ContextError> {
    if let Some(page) = ctx.page_by_url(n) {
        return Ok(page.url());
    }
    let norm = norm_url(Some(n))?;
    Ok(match ctx.page_by_norm(&norm) {
        Some(page) => page.url(),
        None => n.to_owned(),
    })
}

/// `check_pagination_sequence`: PAGINATION_SEQUENCE_ERROR — a break in an
/// otherwise contiguous page-number run.
///
/// Walks the same rel="next" graph as `check_pagination_series` and reads each
/// URL's own page number. What it reports is narrow on purpose: only the
/// *steps* between consecutive pages are judged, never the first number; a
/// stride that covers most steps is legal; every URL must state a number; a
/// cycling series belongs to PAGINATION_LOOP.
pub fn check_pagination_sequence(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let mut next_map: IndexMap<String, String> = IndexMap::new();
    for (page, rec) in pagination_records(ctx)? {
        let nxt = rec.get("rel_next");
        if truthy_opt(nxt) {
            let Some(nxt) = nxt.and_then(Value::as_str) else {
                return Err(ContextError::Value(
                    "expected string for pagination declaration".to_owned(),
                ));
            };
            let from = pagination_key(ctx, &page.url())?;
            let to = pagination_key(ctx, nxt)?;
            next_map.insert(from, to);
        }
    }
    let (reciprocal_judged, reciprocal_breaks) = check_unnumbered_pagination(ctx, &next_map)?;
    if next_map.is_empty() {
        ctx.skip(
            "PAGINATION_SEQUENCE_ERROR",
            "no rel=\"next\" column in Internal:All",
        );
        return Ok(());
    }

    let has_predecessor: HashSet<String> = next_map.values().cloned().collect();
    let heads: Vec<String> = next_map
        .keys()
        .filter(|k| !has_predecessor.contains(*k))
        .cloned()
        .collect();
    if heads.is_empty() {
        // Every chain closes on itself, so not one of them has a first page to
        // walk from. PAGINATION_LOOP reports the cycles; this check reports
        // that it never got a series to read.
        ctx.skip(
            "PAGINATION_SEQUENCE_ERROR",
            "every rel=\"next\" chain in the crawl cycles back on itself, so none of them \
             has a first page to walk from (PAGINATION_LOOP reports the cycles)",
        );
        return Ok(());
    }

    let mut judged = 0usize;
    let mut unjudged: Vec<(String, String)> = Vec::new();
    for start in heads {
        let (path, loops_to) = series_from(&next_map, &start);
        let mut stated = Vec::new();
        for n in &path {
            stated.push(pagination_page_number(&display_url(ctx, n)?));
        }
        if let Some(mut reason) = series_unjudged_reason(&stated, loops_to.is_some()) {
            let resolved_by_reciprocal = loops_to.is_none()
                && stated.iter().any(Option::is_none)
                && path
                    .iter()
                    .all(|node| reciprocal_judged.contains(node) || !next_map.contains_key(node));
            if resolved_by_reciprocal {
                judged += 1;
                continue;
            }
            if ctx.scan_con.is_some() && loops_to.is_none() && stated.iter().any(Option::is_none) {
                reason = "selected native documents do not establish unique reciprocal next/prev \
                     relationships"
                    .to_owned();
            }
            let head = display_url(ctx, &path[0])?;
            unjudged.push((head, reason));
            continue;
        }
        judged += 1;
        let numbers: Vec<i128> = stated.iter().flatten().copied().collect();
        let steps: Vec<i128> = numbers.windows(2).map(|w| w[1] - w[0]).collect();
        let expected = expected_step(&steps);
        let mut breaks = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            if Some(*step) != expected && !reciprocal_breaks.contains(&path[i]) {
                breaks.push(json!({
                    "from": display_url(ctx, &path[i])?,
                    "to": display_url(ctx, &path[i + 1])?,
                    "from_page": numbers[i],
                    "to_page": numbers[i + 1],
                }));
            }
        }
        if breaks.is_empty() {
            continue;
        }
        let mut series = Vec::new();
        for n in &path {
            series.push(display_url(ctx, n)?);
        }
        let target_url = display_url(ctx, &path[0])?;
        ctx.add(
            "PAGINATION_SEQUENCE_ERROR",
            AddOpts {
                target_url: Some(target_url),
                details: {
                    let mut d = Map::new();
                    d.insert("series".into(), json!(series));
                    d.insert("page_numbers".into(), json!(numbers));
                    d.insert("breaks".into(), json!(breaks));
                    d
                },
                ..AddOpts::default()
            },
        )?;
    }
    if !unjudged.is_empty() {
        let reason = unjudged_series_skip_reason(&unjudged, judged);
        ctx.skip("PAGINATION_SEQUENCE_ERROR", &reason);
    }
    Ok(())
}

/// `(judged, reported_forward)` — the pair `_check_unnumbered_pagination`
/// returns.
type PaginationSets = (HashSet<String>, HashSet<String>);

/// `_check_unnumbered_pagination`: judge explicit reciprocal contradictions,
/// never infer opaque page numbers. Returns `(judged, reported_forward)`.
fn check_unnumbered_pagination(
    ctx: &mut AuditContext,
    next_map: &IndexMap<String, String>,
) -> Result<PaginationSets, ContextError> {
    let (_loops, cycling) = pagination_loops(next_map);
    let mut judged: HashSet<String> = HashSet::new();
    let mut reported_forward: HashSet<String> = HashSet::new();
    for (page, rec_obj) in pagination_records(ctx)? {
        let source = pagination_key(ctx, &page.url())?;
        if cycling.contains(&source) {
            continue;
        }
        for (relation, reverse) in [("next", "prev"), ("prev", "next")] {
            let raw_target = rec_obj.get(format!("rel_{relation}").as_str());
            if !truthy_opt(raw_target) {
                continue;
            }
            let Some(target) = raw_target.and_then(Value::as_str) else {
                // `pagination_page_number`/`urlsplit` raise on a non-str,
                // which the check does not catch.
                return Err(ContextError::Value(
                    "expected string for pagination declaration".to_owned(),
                ));
            };
            if ctx.scan_con.is_none() {
                let source_url = display_url(ctx, &source)?;
                if pagination_page_number(&source_url).is_some()
                    && pagination_page_number(target).is_some()
                {
                    continue;
                }
            }
            let key = pagination_key(ctx, target)?;
            if cycling.contains(&key) {
                continue;
            }
            let target_page = match ctx.page_by_url(target) {
                Some(p) => Some(p),
                None => {
                    let norm = norm_url(Some(target))?;
                    ctx.page_by_norm(&norm)
                }
            };
            let Some(target_page) = target_page else {
                continue;
            };
            let mut target_rec = target_page.map(|p| rec(p).cloned().unwrap_or_default());
            if ctx.scan_con.is_some() {
                if pagination_key(ctx, &target_page.url())? != key {
                    continue;
                }
                let Some(declarations) =
                    spiderhead_store::head_links::for_url(con(ctx), &target_page.url())
                        .map_err(|e| ContextError::Value(e.to_string()))?
                else {
                    continue;
                };
                target_rec = pagination_record(&declarations)?;
            }
            let Some(counterpart) = target_rec.get(format!("rel_{reverse}").as_str()) else {
                continue; // a missing statement does not establish a contradiction
            };
            // An explicit null reads back as a missing statement too.
            if counterpart.is_null() {
                continue;
            }
            judged.insert(source.clone());
            // `_pagination_key(ctx, counterpart)` — `norm_url`/`urlsplit` on a
            // non-str raises; a falsy non-str is `(v or "")` → "" and proceeds.
            let counterpart_str = match counterpart {
                Value::String(s) => s.clone(),
                other if !truthy(other) => String::new(),
                _ => {
                    return Err(ContextError::Value(
                        "expected string for pagination declaration".to_owned(),
                    ));
                }
            };
            if pagination_key(ctx, &counterpart_str)? == source {
                continue;
            }
            let reporter = if relation == "next" { &source } else { &key };
            if reported_forward.contains(reporter) {
                continue;
            }
            reported_forward.insert(reporter.clone());
            let reporter_url = display_url(ctx, reporter)?;
            let source_url = display_url(ctx, &source)?;
            let source_annotation = rec_obj
                .get(format!("annotation_{relation}").as_str())
                .cloned()
                .unwrap_or(Value::Null);
            let target_annotation = target_rec
                .get(format!("annotation_{reverse}").as_str())
                .cloned()
                .unwrap_or(Value::Null);
            ctx.add(
                "PAGINATION_SEQUENCE_ERROR",
                AddOpts {
                    target_url: Some(reporter_url),
                    details: {
                        let mut d = Map::new();
                        d.insert("series".into(), json!([source_url, target]));
                        d.insert(
                            "basis".into(),
                            json!("contradictory reciprocal pagination declarations"),
                        );
                        d.insert(
                            "breaks".into(),
                            json!([{
                                "from": source_url,
                                "to": target,
                                "relation": relation,
                                "return_relation": reverse,
                                "expected_return_url": source_url,
                                "declared_return_url": counterpart,
                                "source_annotation": source_annotation,
                                "target_annotation": target_annotation,
                            }]),
                        );
                        d
                    },
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok((judged, reported_forward))
}
