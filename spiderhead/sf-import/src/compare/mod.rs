//! Diff two audit documents into the four finding sets, per check — the in-memory
//! `compare.v1` result and, for corpora too large to hold, the checkpointed
//! `compare.v2` file set in `compare_store`.

pub(crate) mod base;
pub(crate) mod facts;

pub use base::{has_key, header_obj, iter_rows, run_of, CompareError};

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value};
use spiderhead_core::job_control::JobProgressUpdate;
use spiderhead_store::evidence_contract::{comparison_compatibility, comparison_warnings};

use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{
    crawled_urls, load_correspondence, mapped_issues, resolve_correspondence,
};
use crate::pyvalue::py_eq;
use crate::verification::AuditSource;

/// Names of the four difference buckets, in the order reports present them.
pub(crate) const DIRECTIONS: [&str; 4] = ["entered", "left", "appeared", "disappeared"];

/// `dict.fromkeys` order-preserving dedup for the merged warning list.
fn dedup(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

/// How a finding row must be sorted inside a bucket: by check name, then target
/// URL — the same key the finding keys themselves use.
fn bucket_sort(bucket: &mut [Obj]) {
    bucket.sort_by(|a, b| {
        let ka = crate::compare::base::key_of(a);
        let kb = crate::compare::base::key_of(b);
        ka.cmp(&kb)
    });
}

/// Diff two audit documents into the four sets, per check.
///
/// Both documents must carry `pages` and `issues` in the shape this toolkit
/// produces; a document missing either is refused by name rather than silently
/// treated as empty, because an empty crawl and an unreadable one must not look
/// the same in the result. A known difference between the two effective crawl
/// configurations is also refused unless the caller passes `force`: a changed
/// robots policy or URL budget is not evidence that the site itself changed.
/// With `out_dir` the comparison runs through the checkpointed file pipeline,
/// which streams complete rows to disk instead of holding them in memory.
#[allow(clippy::too_many_arguments)]
pub fn compare(
    before_source: &AuditSource<'_>,
    after_source: &AuditSource<'_>,
    force: bool,
    correspondence: Option<&Value>,
    out_dir: Option<&Path>,
    compression: &str,
    resume: bool,
    progress: Option<&dyn Fn(&JobProgressUpdate)>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<Obj, CompareError> {
    if compression != "none" && compression != "gzip" {
        return Err(CompareError(
            "comparison compression must be none or gzip".into(),
        ));
    }
    if resume && out_dir.is_none() {
        return Err(CompareError("comparison resume requires out_dir".into()));
    }
    if compression != "none" && out_dir.is_none() {
        return Err(CompareError(
            "comparison compression requires out_dir".into(),
        ));
    }
    for (label, source) in [("before", before_source), ("after", after_source)] {
        if !has_key(source, "pages") || !has_key(source, "issues") {
            return Err(CompareError(format!(
                "{label} is not an audit.json document (missing pages or issues)"
            )));
        }
    }

    let before_header = header_obj(before_source);
    let after_header = header_obj(after_source);
    let before_cfg = run_of(before_header)
        .get("crawl_config")
        .filter(|v| !v.is_null());
    let after_cfg = run_of(after_header)
        .get("crawl_config")
        .filter(|v| !v.is_null());
    if let (Some(a), Some(b)) = (before_cfg, after_cfg) {
        if !py_eq(a, b) && !force {
            let empty = Map::new();
            let (a_map, b_map) = (
                a.as_object().unwrap_or(&empty),
                b.as_object().unwrap_or(&empty),
            );
            let changed: Vec<&String> = a_map
                .keys()
                .chain(b_map.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(
                    |key| !match (a_map.get(key.as_str()), b_map.get(key.as_str())) {
                        (Some(x), Some(y)) => py_eq(x, y),
                        (None, None) => true,
                        _ => false,
                    },
                )
                .collect();
            return Err(CompareError(format!(
                "results-affecting settings differ between the two runs: {}; pass force=True \
                 only when this comparison is intended",
                changed
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    if let Some(out_dir) = out_dir {
        return crate::compare_store::compare_to_files(
            before_source,
            after_source,
            out_dir,
            force,
            correspondence,
            compression,
            resume,
            progress,
            cancel_event,
        );
    }
    // Beyond this bound a diff does not fit in memory; the file pipeline is the
    // only path that keeps every row.
    if let AuditSource::Reader(reader) = before_source {
        for name in ["pages", "issues"] {
            if reader
                .collections
                .get(&format!("/{name}"))
                .copied()
                .unwrap_or(0)
                > 10_000
            {
                return Err(CompareError(
                    "large audit.v2 comparison requires out_dir for complete file output".into(),
                ));
            }
        }
    }
    if let AuditSource::Reader(reader) = after_source {
        for name in ["pages", "issues"] {
            if reader
                .collections
                .get(&format!("/{name}"))
                .copied()
                .unwrap_or(0)
                > 10_000
            {
                return Err(CompareError(
                    "large audit.v2 comparison requires out_dir for complete file output".into(),
                ));
            }
        }
    }

    let (resolved, before_urls, before_issues);
    match correspondence {
        None => {
            resolved = None;
            before_urls = crawled_urls(before_source)?;
            before_issues = mapped_issues(before_source, None)?;
        }
        Some(declaration) => {
            let value = load_correspondence(declaration)?;
            let mapped = resolve_correspondence(before_source, after_source, &value)?;
            // The baseline is compared under its translated URL set: each
            // before URL counts as the after URL it maps to, itself otherwise.
            before_urls = crawled_urls(before_source)?
                .iter()
                .map(|url| mapped.before_to_after.get(url).unwrap_or(url).clone())
                .collect();
            before_issues = mapped_issues(before_source, Some(&mapped.before_to_after))?;
            resolved = Some(mapped);
        }
    }
    let after_urls = crawled_urls(after_source)?;
    // The after side is never remapped: the baseline was translated onto it.
    let after_issues = mapped_issues(after_source, None)?;
    // A partial baseline cannot prove a URL it never reached is genuinely new —
    // only that it did not see it. Without this, every finding on a URL outside
    // the truncated baseline is misreported as "appeared".
    let before_partial = run_of(header_obj(before_source))
        .get("crawl_partial")
        .is_some_and(crate::pyvalue::truthy);
    // A partial after-crawl leaves its unreached URLs unproven; they are
    // reported as not rechecked, never as fixed.
    let after_partial = run_of(header_obj(after_source))
        .get("crawl_partial")
        .is_some_and(crate::pyvalue::truthy);

    let mut entered: Vec<Obj> = Vec::new();
    let mut left: Vec<Obj> = Vec::new();
    let mut appeared: Vec<Obj> = Vec::new();
    let mut disappeared: Vec<Obj> = Vec::new();

    let all_keys: BTreeSet<(String, String)> = before_issues
        .keys()
        .chain(after_issues.keys())
        .cloned()
        .collect();
    for key in all_keys {
        let url = &key.1;
        let in_before = before_issues.contains_key(&key);
        let in_after = after_issues.contains_key(&key);

        if in_before && in_after {
            continue; // unchanged: matched in both, not a difference
        }

        // An audit-wide finding (no target_url) describes the crawl as a whole
        // rather than a page, so it has no page-presence to test and can only
        // enter or leave — never appear or disappear, which both assert
        // something about a URL's existence.
        if url.is_empty() {
            if in_after && !in_before {
                entered.push(after_issues[&key].clone());
            } else {
                left.push(before_issues[&key].clone());
            }
            continue;
        }

        let url_in_before_crawl = before_urls.contains(url);
        let url_in_after_crawl = after_urls.contains(url);

        if in_after && !in_before {
            let record = after_issues[&key].clone();
            if url_in_before_crawl || before_partial {
                entered.push(record); // existed before, is a new finding now
            } else {
                appeared.push(record); // the URL itself is new to this crawl
            }
        } else if in_before && !in_after {
            let record = before_issues[&key].clone();
            if url_in_before_crawl && url_in_after_crawl {
                left.push(record); // fetched by both crawls, no longer matches — a fix
            } else {
                disappeared.push(record); // not rechecked: not fetched by both crawls
            }
        }
    }

    bucket_sort(&mut entered);
    bucket_sort(&mut left);
    bucket_sort(&mut appeared);
    bucket_sort(&mut disappeared);

    let mut by_check: Map<String, Value> = Map::new();
    for (bucket_name, bucket) in DIRECTIONS.iter().zip([
        entered.iter(),
        left.iter(),
        appeared.iter(),
        disappeared.iter(),
    ]) {
        for item in bucket {
            let check = base_key_check(item);
            let row = by_check.entry(check).or_insert_with(json_counts);
            let Some(row) = row.as_object_mut() else {
                continue; // by_check rows are always objects
            };
            if let Some(count) = row.get_mut(*bucket_name).and_then(|value| value.as_i64()) {
                row.insert((*bucket_name).to_owned(), Value::from(count + 1));
            }
        }
    }

    let before_generated = run_of(header_obj(before_source))
        .get("generated_at")
        .cloned()
        .unwrap_or(Value::Null);
    let after_generated = run_of(header_obj(after_source))
        .get("generated_at")
        .cloned()
        .unwrap_or(Value::Null);

    let warnings = dedup(
        facts::preflight(before_source, after_source)
            .into_iter()
            .chain(comparison_warnings(before_header, after_header))
            .collect(),
    );
    let compatibility: Vec<Value> = comparison_compatibility(before_header, after_header)
        .into_iter()
        .map(Value::Object)
        .collect();

    let mut result = Map::new();
    result.insert("schema_version".into(), Value::String("compare.v1".into()));
    result.insert(
        "before".into(),
        serde_json::json!({
            "generated_at": before_generated,
            "urls_crawled": before_urls.len(),
        }),
    );
    result.insert(
        "after".into(),
        serde_json::json!({
            "generated_at": after_generated,
            "urls_crawled": after_urls.len(),
        }),
    );
    result.insert(
        "warnings".into(),
        Value::Array(warnings.into_iter().map(Value::String).collect()),
    );
    result.insert("compatibility".into(), Value::Array(compatibility));
    result.insert(
        "summary".into(),
        serde_json::json!({
            "entered": entered.len(),
            "left": left.len(),
            "appeared": appeared.len(),
            "disappeared": disappeared.len(),
            "by_check": by_check,
        }),
    );
    for (name, bucket) in DIRECTIONS
        .iter()
        .zip([entered, left, appeared, disappeared])
    {
        result.insert(
            (*name).into(),
            Value::Array(bucket.into_iter().map(Value::Object).collect()),
        );
    }
    result.insert(
        "url_sets".into(),
        facts::url_set_lists(&before_urls, &after_urls, before_partial || after_partial),
    );
    let gaps = facts::measurement_gaps(before_source, after_source);
    if !gaps.is_empty() {
        result.insert(
            "measurement_gaps".into(),
            Value::Array(gaps.into_iter().map(Value::Object).collect()),
        );
    }
    if let Some(mapped) = &resolved {
        let review = facts::release_review(before_source, after_source, mapped, &result, force);
        result.insert("release_review".into(), review);
    }
    Ok(result)
}

/// The `by_check` counter row for one check, all buckets zeroed.
fn json_counts() -> Value {
    serde_json::json!({
        "entered": 0,
        "left": 0,
        "appeared": 0,
        "disappeared": 0,
    })
}

/// The check name a bucket row carries (audit-wide rows key on the empty name).
fn base_key_check(item: &Obj) -> String {
    crate::compare::base::key_of(item).0
}
