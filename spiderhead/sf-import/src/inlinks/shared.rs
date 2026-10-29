//! Shared extraction layer for the inlink checks: export records, the audit
//! graph, and site-scope helpers.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Map, Value};
use spiderhead_core::graph::GraphAccess;
use spiderhead_parse::py_url::urlsplit;

use crate::context::{AddOpts, AuditContext, ContextError, PageHandle};
use crate::normalize::{is_true, norm_url, records_from_df, Object, INLINKS_FIELD_MAP};

/// How many URLs of a shortest route one finding quotes, and how many from
/// each end when the route is longer. The depth is stated in full either way;
/// the two ends identify the route — the middle of a chain is more of the
/// same link.
pub const MAX_PATH_URLS: usize = 20;

/// The bulk `*:Inlinks` exports the status checks read, each with the internal
/// and the external check it feeds.
pub const INLINK_SOURCES: [(&str, &str, &str); 3] = [
    (
        "inlinks_4xx",
        "BROKEN_INTERNAL_LINK",
        "BROKEN_EXTERNAL_LINK",
    ),
    ("inlinks_5xx", "LINK_TO_5XX", "BROKEN_EXTERNAL_LINK"),
    (
        "inlinks_3xx",
        "INTERNAL_LINK_TO_REDIRECT",
        "EXTERNAL_LINK_TO_REDIRECT",
    ),
];

/// One link row of an inlinks export (`models.Link`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Link {
    pub source_url: Option<Value>,
    pub destination_url: Option<Value>,
    pub anchor: Option<Value>,
    pub alt_text: Option<Value>,
    pub status_code: Option<Value>,
    pub link_position: Option<Value>,
    pub link_path: Option<Value>,
    /// `None` when the row carried no Follow value at all.
    pub follow: Option<bool>,
    pub rel: Option<Value>,
    pub target: Option<Value>,
}

/// reference truthiness of an optional cell (missing counts as false).
pub(crate) fn truthy(value: &Option<Value>) -> bool {
    value.as_ref().is_some_and(crate::pyvalue::truthy)
}

/// The cell as a string, when it is one.
pub(crate) fn str_of(value: &Option<Value>) -> Option<&str> {
    value.as_ref().and_then(Value::as_str)
}

impl Link {
    /// `_link_from_record`: the `Link` constructor from one export row.
    pub fn from_record(rec: &Object) -> Link {
        let get = |k: &str| rec.get(k).filter(|v| !v.is_null()).cloned();
        let follow_raw = rec.get("follow").filter(|v| !v.is_null());
        Link {
            source_url: get("source_url"),
            destination_url: get("destination_url"),
            anchor: get("anchor"),
            alt_text: get("alt_text"),
            status_code: get("status_code"),
            link_position: get("link_position"),
            link_path: get("link_path"),
            follow: follow_raw.map(is_true),
            rel: get("rel"),
            target: get("target"),
        }
    }

    /// `as_location`: the per-location issue block quoting this link.
    pub fn as_location(&self) -> Object {
        let mut out = Map::new();
        let put = |out: &mut Object, key: &str, value: &Option<Value>| {
            out.insert(key.to_owned(), value.clone().unwrap_or(Value::Null));
        };
        put(&mut out, "source_url", &self.source_url);
        put(&mut out, "anchor", &self.anchor);
        put(&mut out, "alt_text", &self.alt_text);
        put(&mut out, "link_position", &self.link_position);
        put(&mut out, "link_path", &self.link_path);
        out.insert(
            "follow".into(),
            match self.follow {
                Some(v) => Value::from(v),
                None => Value::Null,
            },
        );
        put(&mut out, "rel", &self.rel);
        put(&mut out, "target", &self.target);
        out
    }
}

/// Lower-cased host of a URL (`urlparse(url).netloc.lower()`).
pub(crate) fn host_of(url: &str) -> String {
    urlsplit(url)
        .map(|s| s.netloc.to_lowercase())
        .unwrap_or_default()
}

/// `_site_host`: the most common netloc across crawled pages; the first host
/// seen wins a tie (Counter.most_common is stable over insertion order).
pub fn site_host(ctx: &mut AuditContext) -> String {
    // (host, count) in first-seen order.
    let mut counts: Vec<(String, i64)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for page in ctx.pages_iter() {
        let host = host_of(&page.url());
        if host.is_empty() {
            continue;
        }
        match index.get(&host) {
            Some(&i) => counts[i].1 += 1,
            None => {
                index.insert(host.clone(), counts.len());
                counts.push((host, 1));
            }
        }
    }
    let mut best: Option<(String, i64)> = None;
    for (host, count) in counts {
        match &best {
            Some((_, n)) if *n >= count => {}
            _ => best = Some((host, count)),
        }
    }
    best.map(|(h, _)| h).unwrap_or_default()
}

/// `_all_inlink_records`: the complete inlink inventory, or `None` when it was
/// never exported (or the export is empty).
pub fn all_inlink_records(ctx: &AuditContext) -> Option<Vec<Object>> {
    let table = ctx.exports.get("all_inlinks")?;
    if table.is_empty() {
        return None;
    }
    Some(records_from_df(table, INLINKS_FIELD_MAP))
}

/// `_rec`: the raw export record a page was built from.
pub(crate) fn rec(page: &PageHandle) -> Object {
    page.map(|p| {
        p.metrics
            .get("_record")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    })
}

/// `_position_breakdown`: occurrences per link position.
pub(crate) fn position_breakdown(links: &[Link]) -> Object {
    let mut counts: IndexMap<String, i64> = IndexMap::new();
    for link in links {
        let pos = str_of(&link.link_position)
            .map(str::to_owned)
            .unwrap_or_else(|| "Unknown".into());
        *counts.entry(pos).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(k, v)| (k, Value::from(v)))
        .collect()
}

/// `_process_export`: one `*:Inlinks` status export, grouped by normalized
/// destination into a finding per destination.
pub fn process_export(
    ctx: &mut AuditContext,
    key: &str,
    internal_check: &str,
    external_check: &str,
    site_host: &str,
) -> Result<(), ContextError> {
    if ctx.scan_con.is_some() {
        // Native internal target statuses are emitted by the retained
        // occurrence pass; this export aggregate does not classify retained
        // external HTTP outcomes.
        ctx.skip(
            external_check,
            "not measured by this native check: external response status aggregates are not projected",
        );
        return Ok(());
    }
    let rows = {
        let Some(df) = ctx.exports.get(key).filter(|df| !df.is_empty()) else {
            let reason = format!("export {key} not available");
            ctx.skip(internal_check, &reason);
            ctx.skip(external_check, &reason);
            return Ok(());
        };
        records_from_df(df, INLINKS_FIELD_MAP)
    };
    let max_locs = ctx
        .config
        .get("output")
        .and_then(Value::as_object)
        .and_then(|o| o.get("max_locations_per_issue"))
        .and_then(Value::as_i64)
        .unwrap_or(200)
        .max(0) as usize;

    // Insertion-ordered grouping: findings appear in first-seen CSV order.
    let mut by_dest: IndexMap<String, Vec<Link>> = IndexMap::new();
    let mut raw_dests: IndexMap<String, Vec<String>> = IndexMap::new();
    for rec in rows {
        let link = Link::from_record(&rec);
        let Some(dest) = str_of(&link.destination_url).map(str::to_owned) else {
            continue;
        };
        if dest.is_empty() {
            continue;
        }
        let dest_key = norm_url(Some(&dest))?;
        by_dest.entry(dest_key.clone()).or_default().push(link);
        let raws = raw_dests.entry(dest_key).or_default();
        if !raws.contains(&dest) {
            raws.push(dest);
        }
    }

    for (dest_key, links) in &by_dest {
        // Report the actual crawled spelling whenever it is known;
        // redirect_map is keyed by the Internal:All Address spelling, so
        // first-seen raw inlink text would make both target_url and final_url
        // depend on CSV row order.
        let raws = &raw_dests[dest_key];
        let crawled = ctx.page_by_norm(dest_key);
        let dest = match &crawled {
            Some(page) => page.url(),
            None => raws
                .iter()
                .min_by(|a, b| (a.to_lowercase(), a.as_str()).cmp(&(b.to_lowercase(), b.as_str())))
                .cloned()
                .unwrap_or_default(),
        };
        let dest_host = host_of(&dest);
        let is_internal = dest_host.is_empty() || dest_host == site_host;
        let check_id = if is_internal {
            internal_check
        } else {
            external_check
        };
        let status = links
            .iter()
            .find(|l| truthy(&l.status_code))
            .and_then(|l| l.status_code.as_ref())
            .and_then(Value::as_i64);
        let mut details = Map::new();
        details.insert(
            "link_position_breakdown".into(),
            Value::Object(position_breakdown(links)),
        );
        details.insert(
            "destination_scope".into(),
            Value::from(if is_internal { "internal" } else { "external" }),
        );
        if check_id == "INTERNAL_LINK_TO_REDIRECT" {
            if let Some(final_url) = ctx.redirect_map.get(dest.as_str()) {
                details.insert("final_url".into(), Value::from(final_url.clone()));
            }
        }
        let locations: Vec<Object> = links.iter().take(max_locs).map(Link::as_location).collect();
        // Count distinct source pages, not raw link occurrences.
        let n_sources = links
            .iter()
            .filter_map(|l| str_of(&l.source_url))
            .filter(|s| !s.is_empty())
            .collect::<std::collections::HashSet<_>>()
            .len() as i64;
        let mut evidence = Map::new();
        evidence.insert(
            "export".into(),
            ctx.exports
                .files
                .get(key)
                .map(|f| Value::from(f.clone()))
                .unwrap_or(Value::Null),
        );
        evidence.insert(
            "raw_destinations".into(),
            Value::Array(raws.iter().map(|r| Value::from(r.clone())).collect()),
        );
        ctx.add(
            check_id,
            AddOpts {
                target_url: Some(dest),
                status_code: status,
                occurrences_count: Some(if n_sources > 0 {
                    n_sources
                } else {
                    links.len() as i64
                }),
                locations,
                details,
                evidence,
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_internal_hyperlink_edges`: (source, destination) pairs for internal,
/// followed `Hyperlink` rows. External destinations and `Follow: false` rows
/// are dropped; a row with no Type value at all is kept — assuming it is a
/// hyperlink is the safer default for this filter.
pub fn internal_hyperlink_edges(
    records: &[Object],
    site_host: &str,
) -> Result<Vec<(String, String)>, ContextError> {
    let mut edges = Vec::new();
    for rec in records {
        let (Some(source), Some(dest)) = (
            rec.get("source_url").and_then(Value::as_str),
            rec.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        match rec.get("type") {
            Some(Value::Null) | None => {}
            Some(t) => {
                if !t
                    .as_str()
                    .map(|s| s.trim().to_lowercase() == "hyperlink")
                    .unwrap_or(false)
                {
                    continue;
                }
            }
        }
        if let Some(follow) = rec.get("follow").filter(|v| !v.is_null()) {
            if !is_true(follow) {
                continue;
            }
        }
        let dest_host = host_of(dest);
        if !dest_host.is_empty() && dest_host != site_host {
            continue;
        }
        edges.push((norm_url(Some(source))?, norm_url(Some(dest))?));
    }
    Ok(edges)
}

/// `_internal_hyperlink_records`: rows describing a hyperlink to this site,
/// nofollow ones included — this projection feeds descriptions of the markup
/// (where links sit, which are written twice), and a nofollow link is as much
/// a part of the template as any other.
pub fn internal_hyperlink_records(records: &[Object], site_host: &str) -> Vec<Object> {
    let mut kept = Vec::new();
    for rec in records {
        let (Some(_source), Some(dest)) = (
            rec.get("source_url").and_then(Value::as_str),
            rec.get("destination_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        match rec.get("type") {
            Some(Value::Null) | None => {}
            Some(t) => {
                if !t
                    .as_str()
                    .map(|s| s.trim().to_lowercase() == "hyperlink")
                    .unwrap_or(false)
                {
                    continue;
                }
            }
        }
        let dest_host = host_of(dest);
        if !dest_host.is_empty() && dest_host != site_host {
            continue;
        }
        kept.push(rec.clone());
    }
    kept
}

/// `urldefrag(url)[0]`: the URL without its fragment.
pub(crate) fn urldefrag(url: &str) -> &str {
    match url.split_once('#') {
        Some((base, _)) => base,
        None => url,
    }
}

/// `statistics.median` of a sorted-or-not list: mean of the two middle values
/// for even lengths.
pub(crate) fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// The stored-graph backend when the audit carries one.
pub(crate) fn graph_access(ctx: &AuditContext) -> Option<&dyn GraphAccess> {
    ctx.graph_access.as_deref()
}
