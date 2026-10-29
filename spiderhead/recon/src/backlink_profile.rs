//! Merge explicit backlink evidence without fetching or guessing an index population.
//!
//! CSV exports, provider samples and offline Common Crawl WAT/domain-edge
//! extracts have different coverage. URL pairs are deduplicated, but each
//! observation retains its own provenance and rel tokens; a missing link in a
//! later sample is never treated as a lost link.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value as Json};
use url::Url;

use crate::dates::iso_date;
use crate::net_client::{normalize_domain, normalize_url, registrable_domain};

mod tables;
use tables::*;
mod normalize;
use normalize::*;
mod import_sources;
pub use import_sources::*;
mod merge;
use merge::*;

/// Maximum imported rows per source.
pub const MAX_ROWS: usize = 100_000;
/// Maximum imported bytes per file.
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;
/// Recognized evidence sources.
pub const SOURCES: &[&str] = &[
    "generic",
    "gsc",
    "ahrefs",
    "majestic",
    "moz",
    "semrush",
    "dataforseo",
    "yandex_webmaster",
    "bing_webmaster",
    "common_crawl",
];

/// Optional history persistence seam; when `None` the profile reports `history: null`.
pub type HistorySink<'a> =
    dyn Fn(&str, &[Json], &[Json], &[Json], Option<&str>, Option<&str>, &str) -> Json + 'a;

/// Analyze explicitly supplied imports and evidence.
#[allow(clippy::too_many_arguments)]
pub fn build_backlink_profile(
    domain: &str,
    imports: Option<&[Json]>,
    evidence: Option<&[Json]>,
    db: Option<&str>,
    project: Option<&str>,
    observed_at: Option<&str>,
    state: Option<&str>,
    follow: Option<&str>,
    target_prefix: Option<&str>,
    min_spam_score: Option<f64>,
    max_spam_score: Option<f64>,
    first_seen_from: Option<&str>,
    first_seen_to: Option<&str>,
    one_per_domain: bool,
    target_status: Option<&Json>,
    limit: i64,
    offset: i64,
    history_sink: Option<&HistorySink<'_>>,
) -> Result<Json, String> {
    let host = normalize_domain(domain);
    if host.is_empty() {
        return Err("domain must be a valid hostname or HTTP(S) URL".to_string());
    }
    if db.is_some() && project.is_some() {
        return Err("pass db or project, not both".to_string());
    }
    if !(1..=MAX_ROWS as i64).contains(&limit) || offset < 0 {
        return Err("limit must be 1..100000 and offset must be nonnegative".to_string());
    }
    if !matches!(state, None | Some("live" | "lost" | "unknown"))
        || !matches!(
            follow,
            None | Some("normal" | "nofollow" | "sponsored" | "ugc" | "unknown")
        )
    {
        return Err("unsupported link state or rel filter".to_string());
    }
    for score in [min_spam_score, max_spam_score].into_iter().flatten() {
        if !score.is_finite() || !(0.0..=100.0).contains(&score) {
            return Err("spam score filters must be in 0..100".to_string());
        }
    }
    if let (Some(min), Some(max)) = (min_spam_score, max_spam_score) {
        if min > max {
            return Err("min_spam_score must not exceed max_spam_score".to_string());
        }
    }
    let start = iso_date(&first_seen_from.map(|s| json!(s)).unwrap_or(Json::Null))?;
    let end = iso_date(&first_seen_to.map(|s| json!(s)).unwrap_or(Json::Null))?;
    if let (Some(start), Some(end)) = (&start, &end) {
        if start > end {
            return Err("first_seen_from must not exceed first_seen_to".to_string());
        }
    }
    // Current UTC time when no stamp is supplied.
    let stamp = observed_at.map(str::to_string).unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let micros = secs / 1_000;
        let seconds = micros / 1_000_000;
        let frac = micros % 1_000_000;
        let days = seconds / 86_400;
        let tod = seconds % 86_400;
        format!(
            "{}T{:02}:{:02}:{:02}.{:06}+00:00",
            crate::dates::civil_from_days(days as i64),
            tod / 3600,
            (tod % 3600) / 60,
            tod % 60,
            frac
        )
    });
    {
        // Stamps must be ISO datetimes with a timezone offset; naive stamps are rejected.
        static WITH_TZ: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)^\d{4}-\d{2}-\d{2}[t ][0-9:.]+(z|[+-]\d{2}:?\d{2})$")
                .unwrap_or_else(|e| panic!("{e}"))
        });
        if !WITH_TZ.is_match(stamp.trim()) {
            return Err("observed_at must include a timezone".to_string());
        }
    }
    // Normalize to UTC with microsecond precision.
    let stamp = match crate::dates::normalize_observed_at(&stamp) {
        Some(s) => s,
        None => return Err("observed_at must be an ISO datetime".to_string()),
    };

    let mut observations: Vec<Json> = Vec::new();
    let mut coverage: Vec<Json> = Vec::new();
    let mut domain_edges: Vec<Json> = Vec::new();
    let mut aggregates: Vec<Json> = Vec::new();
    let mut items: Vec<Json> = evidence.map(|s| s.to_vec()).unwrap_or_default();
    for item in imports.map(|s| s.to_vec()).unwrap_or_default() {
        let source = item["source"].as_str().unwrap_or("");
        let path = PathBuf::from(item["path"].as_str().unwrap_or(""));
        let rows = if source == "common_crawl" && item["format"].as_str() == Some("wat") {
            wat_rows(&path)?
        } else {
            read_csv(&path)?
        };
        let mut enriched = item.clone();
        enriched["rows"] = json!(rows);
        enriched["state"] = json!(if item["complete"].as_bool() == Some(true) {
            "complete"
        } else {
            "partial"
        });
        items.push(enriched);
    }
    for item in &items {
        let source = item["source"].as_str().unwrap_or("");
        if !SOURCES.contains(&source) {
            return Err("unsupported backlink source".to_string());
        }
        let (rows, mut info) = source_rows(item)?;
        info["source"] = json!(source);
        info["scope"] = item
            .get("scope")
            .cloned()
            .unwrap_or_else(|| json!("supplied export or provider sample"));
        info["input_rows"] = json!(rows.len());
        info["rejected_rows"] = json!(0);
        info["out_of_scope_rows"] = json!(0);
        info["unknown_targets"] = json!(0);
        info["whole_web_complete"] = json!(false);
        for row in &rows {
            let fields = mapped(row, item.get("columns"))?;
            if !fields.contains_key("source_url") && fields.contains_key("source_domain") {
                let donor = normalize_domain(fields["source_domain"].as_str().unwrap_or(""));
                let target_raw = fields
                    .get("target_domain")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        if source != "common_crawl" {
                            host.clone()
                        } else {
                            String::new()
                        }
                    });
                let target = normalize_domain(&target_raw);
                if !donor.is_empty() && (target == host || target.ends_with(&format!(".{host}"))) {
                    let edge = json!({
                        "source_domain": donor,
                        "target_domain": target,
                        "source": source,
                        "granularity": "domain",
                        "observed_at": stamp,
                    });
                    if !domain_edges.contains(&edge) {
                        domain_edges.push(edge);
                    }
                } else {
                    info["rejected_rows"] = json!(info["rejected_rows"].as_u64().unwrap_or(0) + 1);
                }
                continue;
            }
            if !fields.contains_key("source_url") && fields.contains_key("target_url") {
                let target = match bl_url(&fields["target_url"]) {
                    Ok(t) => t,
                    Err(_) => {
                        info["rejected_rows"] =
                            json!(info["rejected_rows"].as_u64().unwrap_or(0) + 1);
                        continue;
                    }
                };
                if normalize_domain(target.as_deref().unwrap_or("")) != host {
                    info["out_of_scope_rows"] =
                        json!(info["out_of_scope_rows"].as_u64().unwrap_or(0) + 1);
                    continue;
                }
                let mut metrics = Map::new();
                if let Some(obj) = row.as_object() {
                    for (key, value) in obj {
                        if ["external links", "linking sites", "linked pages"]
                            .contains(&key.to_lowercase().as_str())
                        {
                            metrics.insert(key.clone(), value.clone());
                        }
                    }
                }
                aggregates.push(json!({
                    "source": source,
                    "target_url": target,
                    "granularity": "target_aggregate",
                    "metrics": Json::Object(metrics),
                }));
                continue;
            }
            let obs = match observation(row, source, &stamp, item.get("columns")) {
                Ok(o) => o,
                Err(_) => {
                    info["rejected_rows"] = json!(info["rejected_rows"].as_u64().unwrap_or(0) + 1);
                    continue;
                }
            };
            let target = obs["target_url"].as_str().unwrap_or("").to_string();
            let target_host = normalize_domain(&target);
            if !target.is_empty()
                && target_host != host
                && !target_host.ends_with(&format!(".{host}"))
            {
                info["out_of_scope_rows"] =
                    json!(info["out_of_scope_rows"].as_u64().unwrap_or(0) + 1);
                continue;
            }
            if target.is_empty() {
                info["unknown_targets"] = json!(info["unknown_targets"].as_u64().unwrap_or(0) + 1);
            }
            observations.push(obs);
        }
        if info["rejected_rows"].as_u64().unwrap_or(0) > 0
            || info["unknown_targets"].as_u64().unwrap_or(0) > 0
        {
            info["state"] = json!("partial");
            info["complete"] = json!(false);
        }
        coverage.push(info);
    }
    if observations.len() > MAX_ROWS {
        return Err("merged backlink profile exceeds 100000 observations".to_string());
    }
    let mut normalized_status: BTreeMap<String, Json> = BTreeMap::new();
    if let Some(obj) = target_status.and_then(|t| t.as_object()) {
        for (key, value) in obj {
            if let Ok(Some(url)) = bl_url(&json!(key)) {
                normalized_status.insert(url, value.clone());
            }
        }
    }
    let all_rows = merge(&observations, &normalized_status);
    let total_rows = all_rows.len();
    let measured = coverage.iter().any(|item| {
        item["rows_supplied"].as_bool() == Some(true)
            && (item["input_rows"].as_u64().unwrap_or(0) == 0
                || item["input_rows"].as_u64().unwrap_or(0)
                    > item["rejected_rows"].as_u64().unwrap_or(0)
                        + item["out_of_scope_rows"].as_u64().unwrap_or(0))
    });
    let history = if (db.is_some() || project.is_some()) && measured {
        history_sink.map(|sink| {
            sink(
                &host,
                &all_rows,
                &coverage,
                &domain_edges,
                db,
                project,
                &stamp,
            )
        })
    } else {
        None
    };

    let mut filtered: Vec<Json> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in all_rows {
        let score = row["spam"]["score"].as_f64();
        if let Some(state) = state {
            if row["state"].as_str() != Some(state) {
                continue;
            }
        }
        if let Some(follow) = follow {
            let kinds: Vec<&str> = row["link_types"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str())
                .collect();
            if !kinds.contains(&follow) {
                continue;
            }
        }
        if let Some(prefix) = target_prefix {
            if !row["target_url"].as_str().unwrap_or("").starts_with(prefix) {
                continue;
            }
        }
        if let Some(min) = min_spam_score {
            if score.map(|s| s < min).unwrap_or(true) {
                continue;
            }
        }
        if let Some(max) = max_spam_score {
            if score.map(|s| s > max).unwrap_or(true) {
                continue;
            }
        }
        if let Some(start) = &start {
            if row["first_seen"]
                .as_str()
                .map(|f| f < start.as_str())
                .unwrap_or(true)
            {
                continue;
            }
        }
        if let Some(end) = &end {
            if row["first_seen"]
                .as_str()
                .map(|f| f > end.as_str())
                .unwrap_or(true)
            {
                continue;
            }
        }
        let domain_key = row["referring_domain"].as_str().unwrap_or("").to_string();
        if one_per_domain && seen.contains(&domain_key) {
            continue;
        }
        seen.insert(domain_key);
        filtered.push(row);
    }
    let summary = summary(&filtered, &domain_edges);
    let (offset_u, limit_u) = (offset as usize, limit as usize);
    Ok(json!({
        "ok": measured,
        "format": "spiderhead.backlink-profile.v1",
        "domain": host,
        "observed_at": stamp,
        "state": if !measured {
            "unavailable"
        } else if coverage.is_empty() || coverage.iter().any(|i| i["complete"].as_bool() != Some(true)) {
            "partial"
        } else {
            "complete"
        },
        "coverage": coverage,
        "reason": if measured { Json::Null } else { json!("no measured backlink evidence in the requested scope") },
        "whole_web_complete": false,
        "scope": "explicit source evidence; absent rows do not prove lost links",
        "summary": if !measured {
            Json::Null
        } else {
            let mut s = summary.clone();
            if let Some(o) = s.as_object_mut() {
                o.remove("anchors");
                o.remove("domains");
            }
            s
        },
        "anchors": summary["anchors"].clone(),
        "referring_domains": summary["domains"].clone(),
        "domain_edges": domain_edges,
        "aggregate_rows": aggregates,
        "unfiltered_backlinks": total_rows,
        "rows": filtered.iter().skip(offset_u).take(limit_u).cloned().collect::<Vec<_>>(),
        "history": history.unwrap_or(Json::Null),
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": filtered.len(),
            "has_more": offset_u + limit_u < filtered.len(),
        },
    }))
}

/// Generate an explicit, reviewed disavow selection; never uploads or auto-selects toxicity.
/// When `output` is set, the file is published through a staging file and an exclusive
/// hard link; an existing list is never overwritten.
pub fn export_disavow(
    domains: Option<&[String]>,
    urls: Option<&[String]>,
    output: Option<&str>,
) -> Result<Json, String> {
    let mut normalized: Vec<String> = domains
        .unwrap_or(&[])
        .iter()
        .map(|d| normalize_domain(d))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    normalized.sort();
    if normalized.iter().any(|d| d.is_empty()) {
        return Err("disavow domains must be valid hostnames".to_string());
    }
    let mut normalized_urls: BTreeSet<String> = BTreeSet::new();
    for url in urls.unwrap_or(&[]) {
        match bl_url(&json!(url))? {
            Some(u) => {
                normalized_urls.insert(u);
            }
            None => return Err("disavow URLs must be nonempty".to_string()),
        }
    }
    let mut lines =
        vec!["# Explicit reviewed selection; no search-engine submission was made.".to_string()];
    lines.extend(normalized.iter().map(|d| format!("domain:{d}")));
    lines.extend(normalized_urls.iter().cloned());
    let text = format!("{}\n", lines.join("\n"));
    if let Some(output) = output {
        let target = PathBuf::from(output);
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let staged = parent.join(format!(".disavow-{}", std::process::id()));
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&staged).map_err(|e| e.to_string())?;
            use std::io::Write;
            file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        let result = std::fs::hard_link(&staged, &target);
        let _ = std::fs::remove_file(&staged);
        result.map_err(|e| e.to_string())?;
    }
    Ok(json!({
        "ok": true,
        "text": text,
        "domains": normalized.len(),
        "urls": normalized_urls.len(),
        "output": output,
        "submitted": false,
    }))
}
