//! Anchor-text layer: generic-anchor detection and the anchor-text check.

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::inlinks::shared::{graph_access, str_of, truthy, Link};
use crate::normalize::Object;

/// The curated Russian-and-English vocabulary of non-descriptive anchors.
/// Match the entire normalized anchor after lowercasing, collapsing
/// whitespace, and trimming surrounding punctuation — this catches
/// "Read More" and localized equivalents, but not a meaningful phrase such as
/// "click here to buy" where the remaining words add context.
pub const GENERIC_ANCHORS: &[&str] = &[
    // Russian
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
    // English
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

/// `_GENERIC_ANCHOR_TRIM`: punctuation stripped around a normalized anchor.
const ANCHOR_TRIM: &[char] = &[
    ' ', '\t', '\u{00a0}', '.', ',', ';', ':', '!', '?', '«', '»', '"', '\'', '(', ')', '[', ']',
    '{', '}', '…', '—', '–', '-',
];

/// `_norm_anchor`: lowercase, collapse whitespace runs to single spaces, trim
/// punctuation, trim again.
pub fn norm_anchor(anchor: &str) -> String {
    let lower = anchor.to_lowercase();
    let mut collapsed = String::with_capacity(lower.len());
    let mut last_was_space = false;
    for c in lower.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
        } else {
            collapsed.push(c);
            last_was_space = false;
        }
    }
    collapsed
        .trim_matches(|c| ANCHOR_TRIM.contains(&c))
        .trim()
        .to_owned()
}

fn is_generic(anchor: &str) -> bool {
    GENERIC_ANCHORS.contains(&norm_anchor(anchor).as_str())
}

/// `check_anchor_text`: flag localized non-descriptive anchors such as "here"
/// and "click here". Scans every available `*:Inlinks` export (all_inlinks
/// preferred; the status-code reports as fallback). The fix lives on the
/// *source* page, so one issue is emitted per source URL. With no export and
/// no graph the check skips honestly rather than emit zeros.
pub fn check_anchor_text(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let max_locs = ctx
        .config
        .get("output")
        .and_then(Value::as_object)
        .and_then(|o| o.get("max_locations_per_issue"))
        .and_then(Value::as_i64)
        .unwrap_or(200)
        .max(0) as usize;
    let export_keys: Vec<&str> = ["all_inlinks", "inlinks_4xx", "inlinks_5xx", "inlinks_3xx"]
        .iter()
        .copied()
        .filter(|k| ctx.exports.get(k).map(|df| !df.is_empty()).unwrap_or(false))
        .collect();
    if export_keys.is_empty() {
        if let Some(graph) = graph_access(ctx) {
            let groups: Vec<_> = graph
                .iter_anchor_groups(&|anchor: &str| is_generic(anchor), max_locs)
                .collect();
            for group in groups {
                let mut details = Map::new();
                details.insert(
                    "generic_links".into(),
                    Value::Array(group.generic_links.into_iter().map(Value::Object).collect()),
                );
                let mut evidence = Map::new();
                evidence.insert(
                    "exports".into(),
                    Value::Array(vec![Value::from("all_inlinks")]),
                );
                evidence.insert("files".into(), Value::Array(vec![Value::Null]));
                ctx.add(
                    "GENERIC_ANCHOR_TEXT",
                    AddOpts {
                        target_url: Some(group.source_url),
                        occurrences_count: Some(group.occurrences_count as i64),
                        locations: group.locations,
                        details,
                        evidence,
                        ..Default::default()
                    },
                )?;
            }
            return Ok(());
        }
        ctx.skip(
            "GENERIC_ANCHOR_TEXT",
            "no *:Inlinks export available (export 'All Inlinks' or any *:Inlinks report)",
        );
        return Ok(());
    }

    // (source_url, destination_url, anchor.strip().lower()) of links already
    // counted, so merged exports do not double-report one link.
    let mut seen: std::collections::HashSet<(Option<String>, Option<String>, String)> =
        Default::default();
    let mut by_source: IndexMap<String, Vec<Link>> = IndexMap::new();
    for key in &export_keys {
        let records = {
            let Some(df) = ctx.exports.get(key) else {
                continue;
            };
            crate::normalize::records_from_df(df, crate::normalize::INLINKS_FIELD_MAP)
        };
        for rec in records {
            let link = Link::from_record(&rec);
            if !truthy(&link.anchor) || !truthy(&link.source_url) {
                continue;
            }
            let Some(anchor) = str_of(&link.anchor) else {
                continue;
            };
            if !is_generic(anchor) {
                continue;
            }
            let dedup = (
                str_of(&link.source_url).map(str::to_owned),
                str_of(&link.destination_url).map(str::to_owned),
                anchor.trim().to_lowercase(),
            );
            if !seen.insert(dedup) {
                continue;
            }
            let source = str_of(&link.source_url)
                .map(str::to_owned)
                .unwrap_or_default();
            by_source.entry(source).or_default().push(link);
        }
    }

    for (source, links) in &by_source {
        let capped: Vec<Link> = links.iter().take(max_locs).cloned().collect();
        let generic_links: Vec<Value> = capped
            .iter()
            .map(|link| {
                let mut entry = Map::new();
                entry.insert("anchor".into(), link.anchor.clone().unwrap_or(Value::Null));
                entry.insert(
                    "destination".into(),
                    link.destination_url.clone().unwrap_or(Value::Null),
                );
                entry.insert(
                    "link_position".into(),
                    link.link_position.clone().unwrap_or(Value::Null),
                );
                Value::Object(entry)
            })
            .collect();
        let locations: Vec<Object> = capped.iter().map(Link::as_location).collect();
        let mut details = Map::new();
        details.insert("generic_links".into(), Value::Array(generic_links));
        let mut evidence = Map::new();
        evidence.insert(
            "exports".into(),
            Value::Array(export_keys.iter().map(|k| Value::from(*k)).collect()),
        );
        evidence.insert(
            "files".into(),
            Value::Array(
                export_keys
                    .iter()
                    .map(|k| {
                        ctx.exports
                            .files
                            .get(*k)
                            .map(|f| Value::from(f.clone()))
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ),
        );
        ctx.add(
            "GENERIC_ANCHOR_TEXT",
            AddOpts {
                target_url: Some(source.clone()),
                occurrences_count: Some(links.len() as i64),
                locations,
                details,
                evidence,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}
