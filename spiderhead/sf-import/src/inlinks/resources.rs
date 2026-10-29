//! Resource-safety layer: insecure subresources and pagination declarations.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::inlinks::shared::{all_inlink_records, graph_access, rec};
use crate::normalize::{find_column, norm_url, Object, INTERNAL_FIELD_MAP};
use crate::pyvalue::truthy as value_truthy;

/// `_NON_RESOURCE_LINK_TYPES`: link types that are declarations, not resources
/// a page loads.
const NON_RESOURCE_LINK_TYPES: &[&str] = &[
    "hyperlink",
    "canonical",
    "rel next",
    "rel prev",
    "meta refresh",
    "amphtml",
];

/// `_PAGINATION_LINK_TYPES`: all-inlinks Type value -> the relation spelling a
/// finding quotes.
const PAGINATION_LINK_TYPES: &[(&str, &str)] =
    &[("rel next", "rel=\"next\""), ("rel prev", "rel=\"prev\"")];

const PAGINATION_DECLARATION_CHECKS: [&str; 2] =
    ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"];

/// `check_insecure_subresources`: INSECURE_SUBRESOURCE — an HTTPS page loads a
/// subresource over HTTP. Needs the completed inventory of every page
/// cross-joined with the resources it loads: this reads the non-hyperlink rows
/// (Image, JavaScript, CSS, ...) of the same `all_inlinks` export the other
/// inlink checks already use. A native Security:Mixed Content export answers
/// the same question and is preferred when present (MIXED_CONTENT); this only
/// fills the gap when that report was not exported, and skips rather than
/// double-report when it was.
pub fn check_insecure_subresources(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if ctx.exports.has("security_mixed") {
        ctx.skip(
            "INSECURE_SUBRESOURCE",
            "native Security:Mixed Content export already covers this",
        );
        return Ok(());
    }
    let records = all_inlink_records(ctx);
    let Some(records) = records else {
        let Some(graph) = graph_access(ctx) else {
            ctx.skip(
                "INSECURE_SUBRESOURCE",
                "no all_inlinks export (needed for the resource inventory)",
            );
            return Ok(());
        };
        if !graph.has_resource_type() {
            ctx.skip(
                "INSECURE_SUBRESOURCE",
                "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
            );
            return Ok(());
        }
        // Collect before the mutable pass: the iterator holds the graph borrow.
        let mut by_source: IndexMap<String, Vec<String>> = IndexMap::new();
        for (source, dest, link_type) in graph.iter_resources() {
            if !source.to_lowercase().starts_with("https://") {
                continue;
            }
            if NON_RESOURCE_LINK_TYPES.contains(&link_type.trim().to_lowercase().as_str()) {
                continue;
            }
            if dest.to_lowercase().starts_with("http://") {
                by_source.entry(source).or_default().push(dest);
            }
        }
        emit_insecure_subresources(ctx, by_source)?;
        return Ok(());
    };
    if !records
        .iter()
        .any(|r| r.get("type").is_some_and(value_truthy))
    {
        ctx.skip(
            "INSECURE_SUBRESOURCE",
            "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
        );
        return Ok(());
    }
    let mut by_source: IndexMap<String, Vec<String>> = IndexMap::new();
    for item in &records {
        let (Some(source), Some(dest)) = (
            item.get("source_url").and_then(Value::as_str),
            item.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        if source.is_empty() || dest.is_empty() || !source.to_lowercase().starts_with("https://") {
            continue;
        }
        let link_type = item
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
    emit_insecure_subresources(ctx, by_source)
}

/// The shared emission half of `check_insecure_subresources`.
fn emit_insecure_subresources(
    ctx: &mut AuditContext,
    by_source: IndexMap<String, Vec<String>>,
) -> Result<(), ContextError> {
    for (source, resources) in by_source {
        let unique: Vec<Value> = {
            let mut set: Vec<&str> = resources.iter().map(String::as_str).collect();
            set.sort();
            set.dedup();
            set.into_iter().take(20).map(Value::from).collect()
        };
        let mut details = Map::new();
        details.insert("resources".into(), Value::Array(unique));
        ctx.add(
            "INSECURE_SUBRESOURCE",
            AddOpts {
                target_url: Some(source),
                occurrences_count: Some(resources.len() as i64),
                details,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_pagination_declarations`: source URL -> relation -> declared
/// destinations, in export order. Destinations are de-duplicated per relation
/// through `norm_url`, the same identity the anchor half compares by —
/// de-duplicating by raw string instead would make `/blog/page/2` and
/// `/blog/page/2/` one successor to the anchor half and two to this one, and
/// PAGINATION_MULTIPLE would then tell the operator a crawler must guess
/// between two different successors when there is only one. The first
/// spelling seen is the one reported, so the finding quotes markup that is
/// actually on the page rather than a normalized form of it.
pub(crate) fn pagination_declarations(
    records: &[Object],
) -> Result<IndexMap<String, IndexMap<String, Vec<String>>>, ContextError> {
    // source -> relation -> norm_url -> first raw spelling
    let mut seen: IndexMap<String, IndexMap<String, IndexMap<String, String>>> = IndexMap::new();
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
        let type_key = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let Some(relation) = PAGINATION_LINK_TYPES
            .iter()
            .find(|(t, _)| *t == type_key)
            .map(|(_, rel)| *rel)
        else {
            continue;
        };
        let key = norm_url(Some(dest))?;
        seen.entry(source.to_owned())
            .or_default()
            .entry(relation.to_owned())
            .or_default()
            .entry(key)
            .or_insert_with(|| dest.to_owned());
    }
    let mut out = IndexMap::new();
    for (source, by_relation) in seen {
        let mut relations = IndexMap::new();
        for (relation, targets) in by_relation {
            relations.insert(relation, targets.into_values().collect::<Vec<String>>());
        }
        out.insert(source, relations);
    }
    Ok(out)
}

/// `_anchor_destinations`: source (normalized) -> every destination it reaches
/// with a `Hyperlink` row. Unlike the edge projection this keeps nofollow
/// rows: the question here is whether an `<a href>` to the paginated URL
/// exists on the page at all, and a nofollow anchor is still an anchor.
pub(crate) fn anchor_destinations(
    records: &[Object],
) -> Result<HashMap<String, std::collections::HashSet<String>>, ContextError> {
    let mut out: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for item in records {
        let is_hyperlink = item
            .get("type")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase() == "hyperlink")
            .unwrap_or(false);
        if !is_hyperlink {
            continue;
        }
        let (Some(source), Some(dest)) = (
            item.get("source_url").and_then(Value::as_str),
            item.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        if source.is_empty() || dest.is_empty() {
            continue;
        }
        out.entry(norm_url(Some(source))?)
            .or_default()
            .insert(norm_url(Some(dest))?);
    }
    Ok(out)
}

/// `_light_pagination_multiple`: PAGINATION_MULTIPLE from Internal:All's
/// occurrence-2 columns alone. Mirrors CANONICAL_MULTIPLE: the mere presence
/// of a second occurrence is the finding, whatever its value happens to be.
/// Returns whether the light columns exist at all, so the caller knows whether
/// it can skip the heavier all-inlinks read for this check.
fn light_pagination_multiple(ctx: &mut AuditContext) -> Result<bool, ContextError> {
    let has_light_column = match &ctx.internal_df {
        None => false,
        Some(df) => ["rel_next_2", "rel_prev_2"].iter().any(|field| {
            let Some((_, candidates)) = INTERNAL_FIELD_MAP.iter().find(|(name, _)| name == field)
            else {
                return false;
            };
            find_column(df.columns(), candidates).is_some()
        }),
    };
    if !has_light_column {
        return Ok(false);
    }
    for page in ctx.html_pages() {
        let record = rec(&page);
        for (relation, first_field, second_field) in [
            ("rel=\"next\"", "rel_next", "rel_next_2"),
            ("rel=\"prev\"", "rel_prev", "rel_prev_2"),
        ] {
            let (first, second) = (record.get(first_field), record.get(second_field));
            let (Some(first), Some(second)) = (first, second) else {
                continue;
            };
            if !value_truthy(first) || !value_truthy(second) {
                continue;
            }
            let same = match (first.as_str(), second.as_str()) {
                (Some(f), Some(s)) => norm_url(Some(f))? == norm_url(Some(s))?,
                _ => continue,
            };
            if same {
                continue;
            }
            let mut details = Map::new();
            details.insert("relation".into(), Value::from(relation));
            details.insert("urls".into(), json!([first, second]));
            ctx.add(
                "PAGINATION_MULTIPLE",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(true)
}

/// The native-crawl branch of `check_pagination_declarations`: declarations
/// come from the retained head-annotation store, and the anchor comparison is
/// not measured at all.
fn pagination_declarations_native(ctx: &mut AuditContext) -> Result<(), ContextError> {
    // Read both retained aggregates before the emission pass: the connection
    // borrows the context, and ctx.add inside the loop below would conflict
    // with that borrow living until its last use after the loop.
    let active = {
        let Some(con) = ctx.scan_con.as_ref() else {
            return Ok(());
        };
        spiderhead_store::head_links::active(con).map_err(|e| ContextError::Value(e.to_string()))?
    };
    let eligible_total = {
        let Some(con) = ctx.scan_con.as_ref() else {
            return Ok(());
        };
        spiderhead_store::head_links::eligible_count(con)
            .map_err(|e| ContextError::Value(e.to_string()))?
    };
    let mut measured = 0i64;
    let mut partial = false;
    for (source, annotations) in active {
        let Some(page) = ctx.page_by_url(&source) else {
            continue;
        };
        let eligible = page.map(|p| p.is_html() && p.is_2xx());
        let Some(annotations) = annotations else {
            continue;
        };
        if !eligible {
            continue;
        }
        measured += 1;
        partial |= annotations.len() >= 100;
        for relation in ["next", "prev"] {
            // norm_url -> raw target, first spelling wins (insertion order).
            let mut targets: IndexMap<String, String> = IndexMap::new();
            for row in &annotations {
                let Some(target) = row.get("target").and_then(Value::as_str) else {
                    continue;
                };
                if target.is_empty()
                    || !crate::rules::pagination::rel_has_value(row.get("rel"), relation)
                {
                    continue;
                }
                let key = norm_url(Some(target))?;
                targets.entry(key).or_insert_with(|| target.to_owned());
            }
            if targets.len() > 1 {
                let mut details = Map::new();
                details.insert(
                    "relation".into(),
                    Value::from(format!("rel=\"{relation}\"")),
                );
                details.insert(
                    "urls".into(),
                    Value::Array(targets.into_values().map(Value::from).collect()),
                );
                ctx.add(
                    "PAGINATION_MULTIPLE",
                    AddOpts {
                        target_url: Some(source.clone()),
                        details,
                        ..Default::default()
                    },
                )?;
            }
        }
    }
    if measured == 0 {
        ctx.skip(
            "PAGINATION_MULTIPLE",
            "not measured by this native crawl: raw pagination annotations were not retained",
        );
    }
    if partial || measured < eligible_total {
        ctx.skip(
            "PAGINATION_MULTIPLE",
            "not measured completely by this native crawl: head annotations were unavailable or reached their limit",
        );
    }
    ctx.skip(
        "PAGINATION_URL_NOT_IN_ANCHOR",
        "native head annotation and hyperlink comparison is not measured",
    );
    Ok(())
}

/// `check_pagination_declarations`: PAGINATION_MULTIPLE /
/// PAGINATION_URL_NOT_IN_ANCHOR — the declarations themselves. Both read the
/// one export that carries every rel="next"/rel="prev" declaration beside
/// every anchor on the same page, so both answer from the page's own row set
/// rather than from a whole-site negative: unlike UNLINKED_PAGINATION_SERIES,
/// "this page does not link its own successor" is provable on a partial crawl,
/// because the page in question was crawled and its links are all here.
///
/// Each precondition the pair can fail is declared by name rather than passed
/// over: no export, no Type column to tell a declaration from an anchor, no
/// declarations in it at all.
pub fn check_pagination_declarations(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if ctx.scan_con.is_some() {
        return pagination_declarations_native(ctx);
    }
    // The light path is independent of every precondition below — it reads
    // Internal:All's occurrence-2 columns, not all inlinks — so it runs first,
    // and when it has the columns to answer from PAGINATION_MULTIPLE is no
    // longer among the checks a missing or column-less all_inlinks export
    // skips.
    let light_ran = light_pagination_multiple(ctx)?;
    let skip_checks: &[&str] = if light_ran {
        &["PAGINATION_URL_NOT_IN_ANCHOR"]
    } else {
        &PAGINATION_DECLARATION_CHECKS
    };
    let records = all_inlink_records(ctx);
    if records.is_none() && graph_access(ctx).is_none() {
        for check_id in skip_checks {
            ctx.skip(
                check_id,
                "no all_inlinks export (needed for every rel=\"next\"/rel=\"prev\" \
                 declaration and the anchors beside them)",
            );
        }
        return Ok(());
    }
    // A native crawl arrives here by either route and neither carries a link
    // type: its all-inlinks projection leaves Type unset because the spider
    // records only <a href> hyperlinks, and the SQL-backed graph declares
    // has_resource_type False. Both therefore reach the same verdict as an
    // export written without the column, which is also what the two routes'
    // parity contract requires of every check.
    let Some(records) = records else {
        for check_id in skip_checks {
            ctx.skip(
                check_id,
                "the link inventory carries no link type (needed to tell a \
                 rel=\"next\" declaration from an anchor)",
            );
        }
        return Ok(());
    };
    if !records
        .iter()
        .any(|r| r.get("type").is_some_and(value_truthy))
    {
        for check_id in skip_checks {
            ctx.skip(
                check_id,
                "the link inventory carries no link type (needed to tell a \
                 rel=\"next\" declaration from an anchor)",
            );
        }
        return Ok(());
    }
    let declarations = pagination_declarations(&records)?;
    if declarations.is_empty() {
        for check_id in skip_checks {
            ctx.skip(
                check_id,
                "all_inlinks export contains no rel=\"next\"/rel=\"prev\" rows",
            );
        }
        return Ok(());
    }
    let anchors = anchor_destinations(&records)?;
    if anchors.is_empty() {
        // Declarations but not one Hyperlink row anywhere is a filtered export,
        // not a site with no links on it. Reporting every declaration as
        // un-anchored off that would be a whole crawl of wrong findings.
        ctx.skip(
            "PAGINATION_URL_NOT_IN_ANCHOR",
            "all_inlinks export contains no Hyperlink rows (needed for the page's anchors)",
        );
    }
    for (source, by_relation) in &declarations {
        if !light_ran {
            for (relation, targets) in by_relation {
                if targets.len() > 1 {
                    let mut details = Map::new();
                    details.insert("relation".into(), Value::from(relation.clone()));
                    details.insert(
                        "urls".into(),
                        Value::Array(targets.iter().map(|t| Value::from(t.clone())).collect()),
                    );
                    ctx.add(
                        "PAGINATION_MULTIPLE",
                        AddOpts {
                            target_url: Some(source.clone()),
                            details,
                            ..Default::default()
                        },
                    )?;
                }
            }
        }
        if anchors.is_empty() {
            continue;
        }
        let empty = std::collections::HashSet::new();
        let linked = anchors.get(&norm_url(Some(source))?).unwrap_or(&empty);
        let mut missing: Vec<Value> = Vec::new();
        for (relation, targets) in by_relation {
            for target in targets {
                let linked_hit = linked.contains(&norm_url(Some(target))?);
                if !linked_hit {
                    missing.push(json!({"relation": relation, "url": target}));
                }
            }
        }
        if !missing.is_empty() {
            let mut details = Map::new();
            details.insert(
                "declared_without_an_anchor".into(),
                Value::Array(missing.clone()),
            );
            ctx.add(
                "PAGINATION_URL_NOT_IN_ANCHOR",
                AddOpts {
                    target_url: Some(source.clone()),
                    occurrences_count: Some(missing.len() as i64),
                    details,
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}
