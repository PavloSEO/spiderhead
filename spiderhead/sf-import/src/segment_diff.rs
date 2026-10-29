//! Cross-segment counterpart diff: which pages of a source segment have a
//! counterpart in a target segment, and which do not.
//!
//! Pure: which segment a URL belongs to, and why a URL would be rejected from
//! the crawl, arrive as callables. A translated slug has no mirrored path, so the
//! site's own hreflang declaration is the authority and a mirrored path is only a
//! labelled fallback, enabled once the mirror rate measured on this site is high.
//! Every eligible source page lands in exactly one of five classes, so the five
//! counts always sum to the eligible count; "absent" is refused whenever the
//! target segment may not have been fully reached.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::py_url::{urlsplit, urlunsplit};

use crate::normalize::norm_url;
use crate::pyvalue::{py_repr_value, str_or_dumps, truthy};

/// Schema tag of the diff result.
pub const SCHEMA_VERSION: &str = "segment_diff.v1";

/// Below this share of declared pairs whose paths match, path inference is off
/// entirely: it is only trusted on a site that is overwhelmingly path-mirrored.
pub const MIRROR_RATE_THRESHOLD: f64 = 0.90;

/// The five disjoint classes.
pub const CLASSES: [&str; 5] = [
    "declared",
    "declared_not_crawled",
    "inferred",
    "absent",
    "undetermined",
];

/// The diff cannot be computed as asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SegmentDiffError(pub String);

/// The matching rule of one segment, as plain data.
#[derive(Debug, Clone, Default)]
pub struct SegmentDef {
    /// Segment name.
    pub name: String,
    /// Path prefix.
    pub prefix: String,
    /// Exact host (lower-cased).
    pub host: String,
    /// Regex pattern (names a shape, not a substitution).
    pub pattern: String,
}

impl SegmentDef {
    /// From a declaration object; `name` is required.
    pub fn from_value(raw: &Value) -> Result<Self, SegmentDiffError> {
        let text = |k: &str| {
            raw.get(k)
                .filter(|v| truthy(v))
                .map(str_or_dumps)
                .unwrap_or_default()
        };
        let name = raw
            .get("name")
            .map(str_or_dumps)
            .ok_or_else(|| SegmentDiffError("KeyError: 'name'".into()))?;
        Ok(Self {
            name,
            prefix: text("prefix"),
            host: text("host").to_lowercase(),
            pattern: text("pattern"),
        })
    }
}

/// Maps a URL to its segment name.
pub type SegmentFor<'a> = &'a dyn Fn(&str) -> String;
/// Explains why a URL would be rejected from the crawl ("" when it would not).
pub type Rejection<'a> = &'a dyn Fn(&str) -> String;

fn metrics_of(page: &Value) -> Option<&Map<String, Value>> {
    page.get("metrics").and_then(Value::as_object)
}

fn content_type(page: &Value) -> String {
    let own = page.get("content_type").filter(|v| truthy(v));
    let nested = metrics_of(page)
        .and_then(|m| m.get("content_type"))
        .filter(|v| truthy(v));
    own.or(nested).map(str_or_dumps).unwrap_or_default()
}

fn metrics_get<'a>(page: &'a Value, field: &str) -> Option<&'a Value> {
    page.get(field)
        .or_else(|| metrics_of(page).and_then(|m| m.get(field)))
}

fn has_hreflang_field(page: &Value) -> bool {
    page.get("hreflang").is_some() || metrics_of(page).is_some_and(|m| m.contains_key("hreflang"))
}

/// HTML, fetched with a 2xx, and not withheld from indexing.
fn is_eligible(page: &Value) -> bool {
    if !content_type(page).to_lowercase().contains("html") {
        return false;
    }
    let Some(status) = page.get("status_code").and_then(Value::as_f64) else {
        return false;
    };
    if !(200.0..300.0).contains(&(status.trunc())) {
        return false;
    }
    let robots = |k: &str| metrics_get(page, k).and_then(Value::as_str);
    !robots_directives(&[robots("meta_robots"), robots("x_robots")])
        .iter()
        .any(|d| d == "noindex")
}

fn declared_alternate(page: &Value, target: &str, segment_for: SegmentFor) -> Option<String> {
    let entries = metrics_get(page, "hreflang").and_then(Value::as_array)?;
    for entry in entries {
        if !truthy(entry) {
            continue;
        }
        let url = ["url", "raw_href"]
            .iter()
            .filter_map(|k| entry.get(*k).filter(|v| truthy(v)))
            .map(str_or_dumps)
            .next()
            .unwrap_or_default();
        let url = spiderhead_parse::py_str::py_strip(&url).to_owned();
        if !url.is_empty() && segment_for(&url) == target {
            return Some(url);
        }
    }
    None
}

/// Host without userinfo and port, lower-cased; the port when a non-zero one is present.
fn host_and_port(netloc: &str) -> (String, Option<u32>) {
    let hostport = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let (host, port_text) = match hostport.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => (hostport, None),
        },
        None => match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        },
    };
    let port = port_text
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| *p != 0);
    (host.to_lowercase(), port)
}

/// The candidate counterpart under a naive path or host swap, or `None` when
/// neither segment is defined in a way that makes a mirror computable.
fn mirror(
    url: &str,
    source: &SegmentDef,
    target: &SegmentDef,
) -> Result<Option<String>, SegmentDiffError> {
    let parts = urlsplit(url).map_err(|e| SegmentDiffError(e.to_string()))?;
    let (host, port) = host_and_port(&parts.netloc);
    if !source.prefix.is_empty()
        && !target.prefix.is_empty()
        && parts.path.starts_with(&source.prefix)
    {
        let new_path = format!("{}{}", target.prefix, &parts.path[source.prefix.len()..]);
        let netloc = if target.host.is_empty() {
            parts.netloc.clone()
        } else {
            target.host.clone()
        };
        return Ok(Some(urlunsplit(
            &parts.scheme,
            &netloc,
            &new_path,
            &parts.query,
            &parts.fragment,
        )));
    }
    if !source.host.is_empty() && !target.host.is_empty() && host == source.host {
        let netloc = match port {
            Some(p) => format!("{}:{p}", target.host),
            None => target.host.clone(),
        };
        return Ok(Some(urlunsplit(
            &parts.scheme,
            &netloc,
            &parts.path,
            &parts.query,
            &parts.fragment,
        )));
    }
    Ok(None)
}

fn norm(url: Option<&str>) -> Result<String, SegmentDiffError> {
    norm_url(url).map_err(|e| SegmentDiffError(e.to_string()))
}

/// Share of declared source-to-target pairs whose counterpart sits exactly at
/// the mirrored path, measured on this site's own declarations. `None` means no
/// declared pair existed, which is treated like "measured low".
fn measure_mirror_rate(
    source_pages: &[&Value],
    target: &str,
    segment_for: SegmentFor,
    source: &SegmentDef,
    target_def: &SegmentDef,
) -> Result<(Option<f64>, usize), SegmentDiffError> {
    let (mut total, mut matching) = (0usize, 0usize);
    for page in source_pages {
        let Some(alt) = declared_alternate(page, target, segment_for) else {
            continue;
        };
        total += 1;
        let url = page.get("url").map_or_else(String::new, str_or_dumps);
        let mirrored = mirror(&url, source, target_def)?;
        if norm(mirrored.as_deref())? == norm(Some(&alt))? {
            matching += 1;
        }
    }
    Ok(if total == 0 {
        (None, 0)
    } else {
        (Some(matching as f64 / total as f64), total)
    })
}

/// Percent with no decimals, as `format(x, ".0%")`.
fn percent0(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

/// Classify every eligible page of `source` against `target`.
#[allow(clippy::too_many_arguments)]
pub fn diff_segments(
    pages: &[Value],
    source: &str,
    target: &str,
    segments: &[Value],
    segment_for: SegmentFor,
    rejection: Option<Rejection>,
    segments_only: &[String],
    crawl_partial: bool,
) -> Result<Value, SegmentDiffError> {
    let defs: Vec<SegmentDef> = segments
        .iter()
        .map(SegmentDef::from_value)
        .collect::<Result<_, _>>()?;
    let names: BTreeSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    if names.is_empty() {
        return Err(SegmentDiffError(
            "no segments declared anywhere; scope.segments is empty".into(),
        ));
    }
    let quoted = |s: &str| py_repr_value(&json!(s));
    if source == target {
        return Err(SegmentDiffError(format!(
            "source and target segment are both {}",
            quoted(source)
        )));
    }
    let declared = || py_repr_value(&json!(names.iter().collect::<Vec<_>>()));
    for (label, name) in [("source", source), ("target", target)] {
        if !names.contains(name) {
            return Err(SegmentDiffError(format!(
                "unknown {label} segment {}; declared: {}",
                quoted(name),
                declared()
            )));
        }
    }
    if !pages.is_empty() && !pages.iter().any(has_hreflang_field) {
        return Err(SegmentDiffError(
            "page schema carries no hreflang field at all; the cross-segment diff depends on #357's declared alternates and cannot classify anything without them".into(),
        ));
    }

    let url_of = |p: &Value| p.get("url").map_or_else(String::new, str_or_dumps);
    let mut crawled: BTreeSet<String> = BTreeSet::new();
    for p in pages.iter().filter(|p| p.get("url").is_some_and(truthy)) {
        crawled.insert(norm(Some(&url_of(p)))?);
    }
    let source_pages: Vec<&Value> = pages
        .iter()
        .filter(|p| segment_for(&url_of(p)) == source)
        .collect();
    let eligible: Vec<&Value> = source_pages
        .iter()
        .copied()
        .filter(|p| is_eligible(p))
        .collect();

    let source_def = defs.iter().find(|d| d.name == source).expect("validated");
    let target_def = defs.iter().find(|d| d.name == target).expect("validated");
    let (rate, pairs) =
        measure_mirror_rate(&source_pages, target, segment_for, source_def, target_def)?;
    let (inference, mirror_reason) = match rate {
        None => (
            false,
            format!(
                "no declared hreflang pairs from {} to {} to measure a mirror rate from; path inference stays off",
                quoted(source),
                quoted(target)
            ),
        ),
        Some(r) if r < MIRROR_RATE_THRESHOLD => (
            false,
            format!(
                "measured mirror rate {} over {pairs} declared pair(s), below the {} threshold; path inference is switched off",
                percent0(r),
                percent0(MIRROR_RATE_THRESHOLD)
            ),
        ),
        Some(r) => (
            true,
            format!("measured mirror rate {} over {pairs} declared pair(s); path inference is enabled", percent0(r)),
        ),
    };

    let unreachable = if !segments_only.is_empty() && !segments_only.iter().any(|s| s == target) {
        Some(format!(
            "target segment {} is excluded from this crawl by scope.segments_only; absence cannot be confirmed",
            quoted(target)
        ))
    } else if crawl_partial {
        Some("the crawl is partial and may not have fully reached the target segment; absence cannot be confirmed".to_owned())
    } else {
        None
    };

    let mut counts: Vec<(&str, i64)> = CLASSES.iter().map(|c| (*c, 0)).collect();
    let mut rows: Vec<Value> = Vec::new();
    for page in &eligible {
        let url = url_of(page);
        let (cls, method, counterpart, reason): (&str, &str, Option<String>, Option<String>) =
            if let Some(alt) = declared_alternate(page, target, segment_for) {
                if crawled.contains(&norm(Some(&alt))?) {
                    ("declared", "hreflang", Some(alt), None)
                } else {
                    let rej = rejection.map_or_else(String::new, |r| r(&alt));
                    let tail = if rej.is_empty() {
                        String::new()
                    } else {
                        format!(" (would be rejected: {rej})")
                    };
                    let reason = format!(
                        "hreflang names {} but it was not found among crawled pages{tail}",
                        quoted(&alt)
                    );
                    ("declared_not_crawled", "hreflang", Some(alt), Some(reason))
                }
            } else if !inference {
                (
                    "undetermined",
                    "mirror_disabled",
                    None,
                    Some(mirror_reason.clone()),
                )
            } else {
                match mirror(&url, source_def, target_def)? {
                    None => (
                        "undetermined",
                        "mirror_unavailable",
                        None,
                        Some(format!(
                            "cannot construct a mirrored-path candidate between {} and {} (at least one is defined only by a regex)",
                            quoted(source),
                            quoted(target)
                        )),
                    ),
                    Some(c) if crawled.contains(&norm(Some(&c))?) && segment_for(&c) == target => {
                        ("inferred", "mirrored_path", Some(c), None)
                    }
                    Some(c) if unreachable.is_some() => {
                        ("undetermined", "target_unreachable", Some(c), unreachable.clone())
                    }
                    Some(c) => ("absent", "mirrored_path", Some(c), None),
                }
            };
        counts
            .iter_mut()
            .find(|(k, _)| *k == cls)
            .expect("known class")
            .1 += 1;
        rows.push(json!({
            "url": url, "class": cls, "method": method, "counterpart": counterpart, "reason": reason,
        }));
    }
    let counts_json: Map<String, Value> = counts
        .into_iter()
        .map(|(k, v)| (k.to_owned(), json!(v)))
        .collect();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "source": source,
        "target": target,
        "eligible_pages": eligible.len(),
        "mirror_rate": rate,
        "mirror_pairs": pairs,
        "inference_enabled": inference,
        "mirror_rate_reason": mirror_reason,
        "counts": counts_json,
        "pages": rows,
    }))
}
