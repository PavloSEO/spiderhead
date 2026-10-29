//! Hreflang layer: every hreflang-target check over inlink/declaration
//! evidence.
//!
//! Target observation, unlinked targets, quality, reciprocity, noindex
//! targets, confirmation consistency, and the shared unavailable/native-group
//! helpers they degrade through when export or native evidence is absent.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::{json, Value};
use spiderhead_parse::hreflang_code::code_error;
use spiderhead_parse::parser::meta::robots_directives;

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::inlinks::shared::rec;
use crate::normalize::{norm_url, records_from_df, Object, HREFLANG_FIELD_MAP};
use crate::pyvalue::truthy as value_truthy;

/// One hreflang annotation reduced to what the quality sub-checks read; the
/// raw JSON cells are kept so the details quote the annotation as written.
#[derive(Debug, Clone)]
struct HreflangEntry {
    /// The annotation's language tag (`rec.get("hreflang")` — `null` when
    /// the row carried none).
    hreflang: Value,
    /// The annotation's destination (`rec.get("destination_url")`).
    destination: Value,
}

/// `_hreflang_unavailable`: why an hreflang check cannot answer, in the words
/// of the run that produced it. An export run names the export that would
/// enable the check; a native crawl either retained declarations (no native
/// measure for this check) or retained none at all.
fn hreflang_unavailable(ctx: &AuditContext, sf_reason: &str) -> String {
    if ctx.scan_con.is_none() {
        return sf_reason.to_owned();
    }
    if ctx
        .native_hreflang
        .as_ref()
        .and_then(|n| n.get("declarations"))
        .is_some_and(value_truthy)
    {
        return "not measured by this native crawl".to_owned();
    }
    "the native crawl retained no hreflang declarations".to_owned()
}

/// `_ANSWERED_TARGET_STATES`: a captured page, or a target the post-crawl
/// probe fetched because no link reached it (#987).
fn answered_state(state: Option<&str>) -> bool {
    matches!(state, Some("observed") | Some("probed"))
}

/// `_active_declaration`.
fn active_declaration(item: &Value) -> bool {
    item.get("source_representation_state")
        .and_then(Value::as_str)
        == Some("active")
        && item.get("declaration_state").and_then(Value::as_str) == Some("declared")
}

/// reference `type(code) is int` over a JSON number: integers only — floats and
/// booleans are different reference types, and serde keeps them in different
/// variants.
fn is_json_int(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.is_i64() || v.is_u64())
}

/// `_hreflang_code`: one hreflang value folded for comparison — case only,
/// nothing else. "en" and "en-GB" stay genuinely different annotations.
fn fold_code(value: &Value) -> String {
    match value {
        Value::String(s) => s.trim().to_lowercase(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The native declaration set with its coverage facts, cloned out of the
/// context so a check can hold it across its mutable emission pass (the
/// reference checks iterate `ctx.native_hreflang` while calling `ctx.add`).
struct NativeSnapshot {
    declarations: Vec<Value>,
    coverage_state: Option<String>,
    coverage_reason: Option<String>,
}

fn native_snapshot(ctx: &AuditContext) -> Option<NativeSnapshot> {
    let native = ctx.native_hreflang.as_ref()?;
    if !native.get("declarations").is_some_and(value_truthy) {
        return None;
    }
    let coverage_str = |field: &str| {
        native
            .get("coverage")
            .and_then(|c| c.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Some(NativeSnapshot {
        declarations: native
            .get("declarations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        coverage_state: coverage_str("state"),
        coverage_reason: coverage_str("reason"),
    })
}

/// `_native_hreflang_groups`: group the native relation findings on disk and
/// quote an explicitly bounded sample per group.
///
/// The reference generator interleaves SQL reads with `ctx.add` emissions; here
/// every group is materialized first — the temp table is immutable for the
/// loop's duration, so the results are identical — and the context borrow
/// stays clean.
fn native_hreflang_groups(
    ctx: &AuditContext,
    key: &str,
    predicate: &dyn Fn(&Value) -> bool,
    declarations: &[Value],
) -> Result<Vec<(String, Vec<Value>, i64)>, ContextError> {
    let Some(con) = ctx.disk_pages_con() else {
        // reference reads `ctx._disk_pages.con`, which is unset with in-memory
        // pages; the native path always runs disk-backed, so reaching this
        // is the reference AttributeError rendered as an error.
        return Err(ContextError::Value(
            "native hreflang grouping needs the disk page store".into(),
        ));
    };
    let sql_err = |err: rusqlite::Error| ContextError::Value(err.to_string());
    con.execute(
        "CREATE TEMP TABLE hreflang_findings(seq INTEGER PRIMARY KEY,group_key TEXT,value TEXT)",
        [],
    )
    .map_err(sql_err)?;
    let result = (|| -> Result<Vec<(String, Vec<Value>, i64)>, ContextError> {
        {
            let mut stmt = con
                .prepare("INSERT INTO hreflang_findings(group_key,value) VALUES(?1,?2)")
                .map_err(sql_err)?;
            for item in declarations {
                if !predicate(item) {
                    continue;
                }
                let Some(group_key) = item.get(key).and_then(Value::as_str) else {
                    // reference indexes item[key] directly; a missing or
                    // non-string key aborts the generator.
                    return Err(ContextError::Value(format!(
                        "hreflang declaration lacks a string {key:?}"
                    )));
                };
                let value =
                    serde_json::to_string(item).map_err(|e| ContextError::Value(e.to_string()))?;
                stmt.execute(rusqlite::params![group_key, value])
                    .map_err(sql_err)?;
            }
        }
        con.execute(
            "CREATE INDEX hreflang_findings_group ON hreflang_findings(group_key,seq)",
            [],
        )
        .map_err(sql_err)?;
        let limit = ctx
            .config
            .get("output")
            .and_then(|o| o.get("max_locations_per_issue"))
            .and_then(Value::as_i64)
            .unwrap_or(200);
        let mut out = Vec::new();
        {
            let mut groups = con
                .prepare("SELECT group_key,COUNT(*) FROM hreflang_findings GROUP BY group_key ORDER BY MIN(seq)")
                .map_err(sql_err)?;
            let mut rows = groups.query([]).map_err(sql_err)?;
            while let Some(row) = rows.next().map_err(sql_err)? {
                let group_key: String = row.get(0).map_err(sql_err)?;
                let count: i64 = row.get(1).map_err(sql_err)?;
                let mut values = con
                    .prepare("SELECT value FROM hreflang_findings WHERE group_key=?1 ORDER BY seq LIMIT ?2")
                    .map_err(sql_err)?;
                let mut vrows = values
                    .query(rusqlite::params![group_key, limit])
                    .map_err(sql_err)?;
                let mut items = Vec::new();
                while let Some(vrow) = vrows.next().map_err(sql_err)? {
                    let raw: String = vrow.get(0).map_err(sql_err)?;
                    let item: Value = serde_json::from_str(&raw)
                        .map_err(|e| ContextError::Value(e.to_string()))?;
                    items.push(item);
                }
                out.push((group_key, items, count));
            }
        }
        Ok(out)
    })();
    con.execute("DROP TABLE hreflang_findings", [])
        .map_err(sql_err)?;
    result
}

/// `_native_sample_details`: the bounded sample, plus the pointer to the full
/// collection when the bound cut relations off.
fn native_sample_details(name: &str, entries: Vec<Value>, count: i64) -> Object {
    let mut details = Object::new();
    let len = entries.len() as i64;
    details.insert(name.to_owned(), Value::Array(entries));
    if count > len {
        details.insert("sample_truncated".into(), Value::from(true));
        details.insert("omitted_relations".into(), Value::from(count - len));
        details.insert(
            "full_relations_collection".into(),
            Value::from("/summary/saved_corpus_derivations/internationalization/declarations"),
        );
    }
    details
}

/// `_native_source_groups`: consecutive same-source runs of the active
/// declarations, in stored order (reference `groupby` over the filtered
/// iterator — context evidence is ordered by page/document identity, so one
/// group is bounded by one captured payload).
fn native_source_groups(declarations: &[Value]) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for item in declarations {
        if item
            .get("source_representation_state")
            .and_then(Value::as_str)
            != Some("active")
        {
            continue;
        }
        let Some(source) = item.get("source_url").and_then(Value::as_str) else {
            continue;
        };
        match groups.last_mut() {
            Some((key, rows)) if *key == source => rows.push(item.clone()),
            _ => groups.push((source.to_owned(), vec![item.clone()])),
        }
    }
    groups
}

/// The `evidence.export` block quoting the export file a check answered from.
fn export_evidence(ctx: &AuditContext, key: &str) -> Object {
    let mut evidence = Object::new();
    evidence.insert(
        "export".into(),
        ctx.exports
            .files
            .get(key)
            .map(|f| Value::from(f.clone()))
            .unwrap_or(Value::Null),
    );
    evidence
}

// ---------------------------------------------------------------------------
// hreflang -> broken target
// ---------------------------------------------------------------------------
/// HREFLANG_BROKEN_TARGET — hreflang points at a 3xx/4xx/5xx URL.
///
/// Reads the bulk `All Hreflang` report (one row per hreflang annotation:
/// source → target URL + lang). Each target is matched against the crawl: a
/// target that responds 3xx/4xx/5xx (or carries a Redirect URL) breaks
/// international localization and is flagged on its source page. Targets not
/// in the crawl (external cross-domain hreflang) cannot be classified and are
/// skipped silently. If the export is absent the check skips honestly rather
/// than emit a dead zero.
pub fn check_hreflang_targets(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if let Some(native) = native_snapshot(ctx) {
        let broken = |item: &Value| {
            let Some(target) = item.get("target_observation") else {
                return false;
            };
            let code = target.get("status_code").and_then(Value::as_i64);
            active_declaration(item)
                && answered_state(target.get("state").and_then(Value::as_str))
                && is_json_int(target.get("status_code"))
                && (code.is_some_and(|c| c >= 300)
                    || target.get("redirect_url").is_some_and(value_truthy))
        };
        let groups = native_hreflang_groups(ctx, "source_url", &broken, &native.declarations)?;
        let mut fired = false;
        for (source, rows, count) in groups {
            fired = true;
            let targets: Vec<Value> = rows
                .iter()
                .map(|item| {
                    let observation = item.get("target_observation");
                    json!({
                        "hreflang": item.get("lang").cloned().unwrap_or(Value::Null),
                        "target_url": item.get("target").cloned().unwrap_or(Value::Null),
                        "target_identity": item.get("target_identity").cloned().unwrap_or(Value::Null),
                        "status_code": observation.and_then(|t| t.get("status_code")).cloned().unwrap_or(Value::Null),
                        "redirect_url": observation.and_then(|t| t.get("redirect_url")).cloned().unwrap_or(Value::Null),
                        "source_declaration_id": item.get("source_declaration_id").cloned().unwrap_or(Value::Null),
                        "label_context": item.get("label_context").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            let details = native_sample_details("broken_targets", targets, count);
            ctx.add(
                "HREFLANG_BROKEN_TARGET",
                AddOpts {
                    target_url: Some(source),
                    occurrences_count: Some(count),
                    details,
                    ..Default::default()
                },
            )?;
        }
        let unmeasured = native.declarations.iter().any(|item| {
            let target = item.get("target_observation");
            active_declaration(item)
                && (!answered_state(target.and_then(|t| t.get("state")).and_then(Value::as_str))
                    || !is_json_int(target.and_then(|t| t.get("status_code"))))
        });
        if !fired && (unmeasured || native.coverage_state.as_deref() != Some("complete")) {
            ctx.skip(
                "HREFLANG_BROKEN_TARGET",
                "one or more hreflang targets or the retained population were unmeasured",
            );
        }
        return Ok(());
    }

    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)",
            );
            ctx.skip("HREFLANG_BROKEN_TARGET", &reason);
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    let mut by_source: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for item in &records {
        let (Some(src), Some(dest)) = (
            item.get("source_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            item.get("destination_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        // #176 audit: this reads the single 2xx-preferring representative, not
        // every variant under the key, but that stays correct here — unlike
        // the canonical-to-redirect bug, a 3xx and a 4xx twin agree on the
        // verdict this check cares about (both are "broken"), so which one is
        // picked only changes which status/redirect_url gets quoted, never
        // whether the finding fires.
        let Some(target) = ctx.page_by_norm(&norm_url(Some(dest))?) else {
            continue; // external / not crawled — cannot classify the target
        };
        let code = target.map(|p| p.status_code);
        let redirect_url = ctx.redirect_map.get(&target.url()).cloned();
        let is_redirect = code.is_some_and(|c| (300..=399).contains(&c)) || redirect_url.is_some();
        let is_error = code.is_some_and(|c| c >= 400);
        if !(is_redirect || is_error) {
            continue;
        }
        if !seen_pairs.insert((src.to_owned(), dest.to_owned())) {
            continue;
        }
        by_source.entry(src.to_owned()).or_default().push(json!({
            "hreflang": item.get("hreflang").cloned().unwrap_or(Value::Null),
            "target_url": dest,
            "status_code": code.map(Value::from).unwrap_or(Value::Null),
            "redirect_url": redirect_url.map(Value::from).unwrap_or(Value::Null),
        }));
    }
    for (source, targets) in &by_source {
        let count = targets.len() as i64;
        let mut details = Object::new();
        details.insert("broken_targets".into(), Value::Array(targets.clone()));
        ctx.add(
            "HREFLANG_BROKEN_TARGET",
            AddOpts {
                target_url: Some(source.clone()),
                occurrences_count: Some(count),
                details,
                evidence: export_evidence(ctx, "all_hreflang"),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// HREFLANG_UNLINKED_TARGET — an hreflang alternate no ordinary hyperlink
/// reaches.
///
/// A URL that is only ever named by another page's `<link rel="alternate"
/// hreflang>` is discoverable by a search engine following the annotation,
/// but never by a user or crawler following a link. This is the hreflang twin
/// of the unlinked-canonical check: the same set difference, with hreflang
/// targets in place of canonical targets. The target's own Inlinks count is
/// read from Internal:All, which counts hyperlinks only, so hreflang
/// declarations never count as inlinks. A page that names itself is not an
/// unlinked alternate. On a partial crawl "no inlink" cannot be proven, so
/// the aggregate pass withholds this finding.
pub fn check_hreflang_unlinked_targets(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)",
            );
            ctx.skip("HREFLANG_UNLINKED_TARGET", &reason);
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    let has_inlinks_column = ctx
        .pages_iter()
        .iter()
        .any(|page| rec(page).get("inlinks").is_some_and(|v| !v.is_null()));
    if !has_inlinks_column {
        ctx.skip(
            "HREFLANG_UNLINKED_TARGET",
            "no Inlinks column in Internal:All",
        );
        return Ok(());
    }
    let mut declared_from: IndexMap<String, Vec<String>> = IndexMap::new();
    for item in &records {
        let (Some(src), Some(dest)) = (
            item.get("source_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            item.get("destination_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let source_norm = norm_url(Some(src))?;
        let target_norm = norm_url(Some(dest))?;
        if source_norm == target_norm {
            continue; // a self-reference is not evidence that anything links here
        }
        let sources = declared_from.entry(target_norm).or_default();
        if !sources.contains(&src.to_owned()) {
            sources.push(src.to_owned());
        }
    }
    for (target_norm, sources) in &declared_from {
        // Targets outside the crawl cannot be classified, as in the broken
        // target check.
        let Some(target) = ctx.page_by_norm(target_norm) else {
            continue;
        };
        let page_rec = rec(&target);
        if page_rec.get("crawl_depth").and_then(Value::as_i64) == Some(0) {
            continue; // the homepage is never "unlinked"
        }
        // `inlinks is None or inlinks > 0`: no count, or a positive one.
        let Some(inlinks) = page_rec.get("inlinks").and_then(Value::as_i64) else {
            continue;
        };
        if inlinks > 0 {
            continue;
        }
        let mut sorted_sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        sorted_sources.sort();
        let mut details = Object::new();
        details.insert(
            "declared_from".into(),
            Value::Array(sorted_sources.into_iter().map(Value::from).collect()),
        );
        ctx.add(
            "HREFLANG_UNLINKED_TARGET",
            AddOpts {
                target_url: Some(target.url()),
                details,
                evidence: export_evidence(ctx, "all_hreflang"),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

const HREFLANG_QUALITY_CHECKS: [&str; 5] = [
    "HREFLANG_INVALID_CODE",
    "HREFLANG_MULTIPLE_ENTRIES",
    "HREFLANG_MISSING_SELF_REFERENCE",
    "HREFLANG_MISSING_XDEFAULT",
    "HREFLANG_NOT_CANONICAL",
];

/// check_hreflang_quality: validate each page's own hreflang set — codes,
/// duplicates, self, x-default, canonical.
///
/// Reads the same bulk `All Hreflang` report as the broken-target check and
/// reuses the ISO 639-1/3166-1 validator the single-URL hreflang tool ships,
/// instead of re-implementing it. Every check here groups by the declaring
/// page (source), matching how a browser or crawler reads one page's hreflang
/// set. If the export is absent, all five checks skip honestly rather than
/// emit dead zeros.
pub fn check_hreflang_quality(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if let Some(native) = native_snapshot(ctx) {
        for (source, rows) in native_source_groups(&native.declarations) {
            let entries: Vec<HreflangEntry> = rows
                .iter()
                .map(|row| HreflangEntry {
                    hreflang: row.get("lang").cloned().unwrap_or(Value::Null),
                    destination: row.get("target").cloned().unwrap_or(Value::Null),
                })
                .collect();
            let no_evidence = Object::new();
            check_invalid_codes(ctx, &source, &entries, &no_evidence)?;
            check_duplicate_entries(ctx, &source, &entries, &no_evidence)?;
            let targets: Vec<HreflangEntry> = entries
                .iter()
                .filter(|e| value_truthy(&e.destination))
                .cloned()
                .collect();
            check_self_reference(ctx, &source, &targets, &no_evidence)?;
            check_xdefault(ctx, &source, &targets, &no_evidence)?;
            check_not_canonical(ctx, &source, &targets, &no_evidence)?;
        }
        return Ok(());
    }
    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
            );
            for check_id in HREFLANG_QUALITY_CHECKS {
                ctx.skip(check_id, &reason);
            }
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    let mut by_source: IndexMap<String, Vec<HreflangEntry>> = IndexMap::new();
    for item in &records {
        let Some(src) = item
            .get("source_url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        by_source
            .entry(src.to_owned())
            .or_default()
            .push(HreflangEntry {
                hreflang: item.get("hreflang").cloned().unwrap_or(Value::Null),
                destination: item.get("destination_url").cloned().unwrap_or(Value::Null),
            });
    }
    let evidence = export_evidence(ctx, "all_hreflang");
    for (source, entries) in &by_source {
        check_invalid_codes(ctx, source, entries, &evidence)?;
        check_duplicate_entries(ctx, source, entries, &evidence)?;
        let targets: Vec<HreflangEntry> = entries
            .iter()
            .filter(|e| value_truthy(&e.destination))
            .cloned()
            .collect();
        check_self_reference(ctx, source, &targets, &evidence)?;
        check_xdefault(ctx, source, &targets, &evidence)?;
        check_not_canonical(ctx, source, &targets, &evidence)?;
    }
    Ok(())
}

/// `_check_invalid_codes`: HREFLANG_INVALID_CODE — tags the ISO validator
/// rejects.
fn check_invalid_codes(
    ctx: &mut AuditContext,
    source: &str,
    entries: &[HreflangEntry],
    evidence: &Object,
) -> Result<(), ContextError> {
    let mut invalid: Vec<Value> = Vec::new();
    for entry in entries {
        let Some(lang) = entry.hreflang.as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(reason) = code_error(lang) else {
            continue;
        };
        invalid.push(json!({
            "hreflang": entry.hreflang,
            "destination": entry.destination,
            "reason": reason,
        }));
    }
    if !invalid.is_empty() {
        let count = invalid.len() as i64;
        let mut details = Object::new();
        details.insert("invalid".into(), Value::Array(invalid));
        ctx.add(
            "HREFLANG_INVALID_CODE",
            AddOpts {
                target_url: Some(source.to_owned()),
                occurrences_count: Some(count),
                details,
                evidence: evidence.clone(),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_check_duplicate_entries`: HREFLANG_MULTIPLE_ENTRIES — the same tag twice.
///
/// Language tags are case-insensitive: "en-US" declared twice (even with
/// different casing) is the same annotation twice, not two annotations.
fn check_duplicate_entries(
    ctx: &mut AuditContext,
    source: &str,
    entries: &[HreflangEntry],
    evidence: &Object,
) -> Result<(), ContextError> {
    let folded: Vec<String> = entries
        .iter()
        .filter(|e| e.hreflang.as_str().is_some_and(|s| !s.is_empty()))
        .map(|e| fold_code(&e.hreflang))
        .collect();
    let mut duplicates: Vec<String> = folded
        .iter()
        .filter(|lang| folded.iter().filter(|other| *other == *lang).count() > 1)
        .cloned()
        .collect();
    duplicates.sort();
    duplicates.dedup();
    if !duplicates.is_empty() {
        let count = duplicates.len() as i64;
        let mut details = Object::new();
        details.insert(
            "duplicate_values".into(),
            Value::Array(duplicates.into_iter().map(Value::from).collect()),
        );
        ctx.add(
            "HREFLANG_MULTIPLE_ENTRIES",
            AddOpts {
                target_url: Some(source.to_owned()),
                occurrences_count: Some(count),
                details,
                evidence: evidence.clone(),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_check_self_reference`: HREFLANG_MISSING_SELF_REFERENCE — a set that never
/// names its own page.
fn check_self_reference(
    ctx: &mut AuditContext,
    source: &str,
    entries: &[HreflangEntry],
    evidence: &Object,
) -> Result<(), ContextError> {
    let source_norm = norm_url(Some(source))?;
    let mut destinations: HashSet<String> = HashSet::new();
    for entry in entries {
        let Some(dest) = entry.destination.as_str() else {
            continue;
        };
        destinations.insert(norm_url(Some(dest))?);
    }
    if !destinations.contains(&source_norm) {
        let declared: BTreeSet<&str> = entries
            .iter()
            .filter_map(|e| e.destination.as_str())
            .collect();
        let mut details = Object::new();
        details.insert(
            "declared_targets".into(),
            Value::Array(declared.into_iter().map(Value::from).collect()),
        );
        ctx.add(
            "HREFLANG_MISSING_SELF_REFERENCE",
            AddOpts {
                target_url: Some(source.to_owned()),
                details,
                evidence: evidence.clone(),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_check_xdefault`: HREFLANG_MISSING_XDEFAULT — no fallback annotation.
fn check_xdefault(
    ctx: &mut AuditContext,
    source: &str,
    entries: &[HreflangEntry],
    evidence: &Object,
) -> Result<(), ContextError> {
    let folded: HashSet<String> = entries
        .iter()
        .filter(|e| e.hreflang.as_str().is_some_and(|s| !s.is_empty()))
        .map(|e| fold_code(&e.hreflang))
        .collect();
    if !folded.contains("x-default") {
        ctx.add(
            "HREFLANG_MISSING_XDEFAULT",
            AddOpts {
                target_url: Some(source.to_owned()),
                evidence: evidence.clone(),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

/// `_check_not_canonical`: HREFLANG_NOT_CANONICAL — a target whose canonical
/// points elsewhere. Both the finding and the unmeasured skip can fire for
/// one source when only some targets carry measured annotations.
fn check_not_canonical(
    ctx: &mut AuditContext,
    source: &str,
    entries: &[HreflangEntry],
    evidence: &Object,
) -> Result<(), ContextError> {
    let mut offenders: Vec<Value> = Vec::new();
    let mut unmeasured = 0i64;
    for entry in entries {
        let Some(dest) = entry.destination.as_str() else {
            continue;
        };
        let dest_norm = norm_url(Some(dest))?;
        // #176 audit: correct by construction. A redirecting twin under this
        // key rarely carries a canonical tag of its own (the parser reads
        // canonical off the fetched HTML, and a 3xx has none) — the 2xx
        // variant is the one this check needs.
        let target = match ctx.page_by_url(dest) {
            Some(page) => Some(page),
            None => ctx.page_by_norm(&dest_norm),
        };
        let (canonical, body_unavailable) = match &target {
            Some(page) => {
                let page_rec = rec(page);
                (
                    page_rec.get("canonical").cloned(),
                    page_rec.get("body_unavailable").is_some_and(value_truthy),
                )
            }
            None => (
                ctx.target_probes
                    .get(&dest_norm)
                    .and_then(|probe| probe.get("canonical"))
                    .cloned(),
                false,
            ),
        };
        // the reference `canonical is None` reads both a missing probe and an
        // explicit null as "no statement".
        let canonical = canonical.filter(|v| !v.is_null());
        if (target.is_none() && canonical.is_none()) || (target.is_some() && body_unavailable) {
            unmeasured += 1;
            continue;
        }
        if let Some(canonical) = canonical.as_ref().filter(|v| value_truthy(v)) {
            let Some(canonical_str) = canonical.as_str() else {
                continue;
            };
            if norm_url(Some(canonical_str))? != dest_norm {
                offenders.push(json!({
                    "hreflang": entry.hreflang,
                    "destination": entry.destination,
                    "canonical": canonical,
                }));
            }
        }
    }
    if !offenders.is_empty() {
        let count = offenders.len() as i64;
        let mut details = Object::new();
        details.insert(
            "non_canonical_targets".into(),
            Value::Array(std::mem::take(&mut offenders)),
        );
        ctx.add(
            "HREFLANG_NOT_CANONICAL",
            AddOpts {
                target_url: Some(source.to_owned()),
                occurrences_count: Some(count),
                details,
                evidence: evidence.clone(),
                ..Default::default()
            },
        )?;
    }
    if unmeasured > 0 {
        ctx.skip(
            "HREFLANG_NOT_CANONICAL",
            &format!(
                "{unmeasured} hreflang target(s) lack measured canonical annotations; \
                 not measured by this native crawl or export"
            ),
        );
    }
    Ok(())
}

/// HREFLANG_MISSING_RETURN_LINK — A names B, but B never names A back.
///
/// The hreflang contract requires every annotation to be reciprocal: if A
/// points to B, B must point back to A. Whether B reciprocates is a property
/// of the *pair* and depends on B's own hreflang set, which is only on hand
/// once B itself has been crawled — provable only once the crawl of both
/// sides is complete.
pub fn check_hreflang_reciprocity(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if let Some(native) = native_snapshot(ctx) {
        let missing = |item: &Value| {
            active_declaration(item)
                && item.get("source_identity").and_then(Value::as_str)
                    != item.get("target_identity").and_then(Value::as_str)
                && item
                    .get("reciprocity")
                    .and_then(|r| r.get("state"))
                    .and_then(Value::as_str)
                    == Some("missing")
        };
        let groups =
            native_hreflang_groups(ctx, "target_identity", &missing, &native.declarations)?;
        let mut fired = false;
        for (target_identity, entries, count) in groups {
            fired = true;
            let relations: Vec<Value> = entries
                .iter()
                .map(|item| {
                    json!({
                        "source_url": item.get("source_url").cloned().unwrap_or(Value::Null),
                        "source_identity": item.get("source_identity").cloned().unwrap_or(Value::Null),
                        "target": item.get("target").cloned().unwrap_or(Value::Null),
                        "target_identity": item.get("target_identity").cloned().unwrap_or(Value::Null),
                        "hreflang": item.get("lang").cloned().unwrap_or(Value::Null),
                        "source_declaration_id": item.get("source_declaration_id").cloned().unwrap_or(Value::Null),
                        "label_context": item.get("label_context").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            let mut details = native_sample_details("relations", relations, count);
            let mut expected: Vec<&str> = entries
                .iter()
                .filter_map(|item| item.get("source_url").and_then(Value::as_str))
                .collect();
            expected.sort();
            expected.dedup();
            details.insert(
                "expected_return_to".into(),
                Value::Array(expected.into_iter().map(Value::from).collect()),
            );
            let target = ctx.page_by_norm(&target_identity);
            let target_url = match &target {
                Some(page) => page.url(),
                None => entries
                    .first()
                    .and_then(|item| item.get("target"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            };
            ctx.add(
                "HREFLANG_MISSING_RETURN_LINK",
                AddOpts {
                    target_url: Some(target_url),
                    occurrences_count: Some(count),
                    details,
                    ..Default::default()
                },
            )?;
        }
        if !fired && native.coverage_state.as_deref() != Some("complete") {
            ctx.skip(
                "HREFLANG_MISSING_RETURN_LINK",
                &format!(
                    "retained hreflang relationship coverage is incomplete: {}",
                    native.coverage_reason.as_deref().unwrap_or("")
                ),
            );
        }
        return Ok(());
    }

    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
            );
            ctx.skip("HREFLANG_MISSING_RETURN_LINK", &reason);
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    let mut edges: HashSet<(String, String)> = HashSet::new();
    for item in &records {
        let (Some(src), Some(dest)) = (
            item.get("source_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            item.get("destination_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let src_norm = norm_url(Some(src))?;
        let dest_norm = norm_url(Some(dest))?;
        if src_norm == dest_norm {
            continue; // a self-reference is not a reciprocity pair
        }
        edges.insert((src_norm, dest_norm));
    }

    let mut missing_by_target: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut unmeasured = 0i64;
    let mut sorted_edges: Vec<(String, String)> = edges.iter().cloned().collect();
    sorted_edges.sort();
    for (src_norm, dest_norm) in &sorted_edges {
        if edges.contains(&(dest_norm.clone(), src_norm.clone())) {
            continue; // B already names A back
        }
        // #176 audit: every read of page_by_norm below is "does this key exist
        // in the crawl" or a display label — the reciprocity verdict itself
        // comes entirely from the hreflang edge set above, so which variant is
        // the representative is moot.
        let Some(target) = ctx.page_by_norm(dest_norm) else {
            unmeasured += 1;
            continue; // external / not crawled — cannot fault it for not reciprocating
        };
        let complete = target.map(|p| p.is_2xx() && p.is_html());
        let body_unavailable = rec(&target)
            .get("body_unavailable")
            .is_some_and(value_truthy);
        if !complete || body_unavailable {
            unmeasured += 1;
            continue; // no complete target declaration observation
        }
        missing_by_target
            .entry(dest_norm.clone())
            .or_default()
            .push(src_norm.clone());
    }

    let emission: Vec<(String, Vec<String>)> = missing_by_target.into_iter().collect();
    for (dest_norm, source_norms) in &emission {
        let target = ctx.page_by_norm(dest_norm);
        let mut expected_from: Vec<String> = source_norms
            .iter()
            .map(|norm| {
                ctx.page_by_norm(norm)
                    .map(|page| page.url())
                    .unwrap_or_else(|| norm.clone())
            })
            .collect();
        expected_from.sort();
        let count = expected_from.len() as i64;
        let mut details = Object::new();
        details.insert(
            "expected_return_to".into(),
            Value::Array(expected_from.into_iter().map(Value::from).collect()),
        );
        ctx.add(
            "HREFLANG_MISSING_RETURN_LINK",
            AddOpts {
                target_url: Some(target.map(|p| p.url()).unwrap_or_default()),
                occurrences_count: Some(count),
                details,
                evidence: export_evidence(ctx, "all_hreflang"),
                ..Default::default()
            },
        )?;
    }
    if emission.is_empty() && unmeasured > 0 {
        ctx.skip(
            "HREFLANG_MISSING_RETURN_LINK",
            "one or more hreflang targets lack a complete crawled HTML observation",
        );
    }
    Ok(())
}

/// Flag observed noindex alternates; unknown targets stay unmeasured.
pub fn check_hreflang_noindex_targets(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let native = native_snapshot(ctx);
    let mut by_source: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut unmeasured = 0i64;
    if let Some(native) = native {
        let noindex = |item: &Value| {
            let Some(target) = item.get("target_observation") else {
                return false;
            };
            active_declaration(item)
                && answered_state(target.get("state").and_then(Value::as_str))
                && target
                    .get("indexability")
                    .and_then(|i| i.get("reason"))
                    .and_then(Value::as_str)
                    == Some("noindex directive")
        };
        let groups = native_hreflang_groups(ctx, "source_url", &noindex, &native.declarations)?;
        let mut fired = false;
        for (source, rows, count) in groups {
            fired = true;
            let entries: Vec<Value> = rows
                .iter()
                .map(|item| {
                    json!({
                        "hreflang": item.get("lang").cloned().unwrap_or(Value::Null),
                        "target": item.get("target").cloned().unwrap_or(Value::Null),
                        "target_identity": item.get("target_identity").cloned().unwrap_or(Value::Null),
                        "source_declaration_id": item.get("source_declaration_id").cloned().unwrap_or(Value::Null),
                        "label_context": item.get("label_context").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            let details = native_sample_details("noindex_targets", entries, count);
            ctx.add(
                "HREFLANG_NOINDEX_TARGET",
                AddOpts {
                    target_url: Some(source),
                    occurrences_count: Some(count),
                    details,
                    ..Default::default()
                },
            )?;
        }
        let any_unmeasured = native.declarations.iter().any(|item| {
            let target = item.get("target_observation");
            active_declaration(item)
                && (!answered_state(target.and_then(|t| t.get("state")).and_then(Value::as_str))
                    || target
                        .and_then(|t| t.get("indexability"))
                        .and_then(|i| i.get("state"))
                        .and_then(Value::as_str)
                        == Some("unmeasured"))
        });
        if !fired && (any_unmeasured || native.coverage_state.as_deref() != Some("complete")) {
            ctx.skip(
                "HREFLANG_NOINDEX_TARGET",
                "one or more hreflang targets or the retained population were unmeasured",
            );
        }
        return Ok(());
    }

    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(ctx, "no all_hreflang declaration export");
            ctx.skip("HREFLANG_NOINDEX_TARGET", &reason);
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    for item in &records {
        let (Some(source), Some(destination)) = (
            item.get("source_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            item.get("destination_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let dest_norm = norm_url(Some(destination))?;
        let Some(target) = ctx.page_by_norm(&dest_norm) else {
            unmeasured += 1;
            continue;
        };
        let observed = rec(&target);
        let reason = observed
            .get("indexability_status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let directives = robots_directives(&[
            observed.get("meta_robots").and_then(Value::as_str),
            observed.get("x_robots").and_then(Value::as_str),
        ]);
        if reason.starts_with("noindex")
            || reason == "none"
            || directives.iter().any(|d| d == "noindex")
        {
            by_source.entry(source.to_owned()).or_default().push(json!({
                "hreflang": item.get("hreflang").cloned().unwrap_or(Value::Null),
                "target": destination,
                "target_identity": dest_norm,
            }));
        } else if observed
            .get("indexability")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase()
            != "indexable"
        {
            unmeasured += 1;
        }
    }
    for (source, entries) in &by_source {
        let count = entries.len() as i64;
        let mut details = Object::new();
        details.insert("noindex_targets".into(), Value::Array(entries.clone()));
        ctx.add(
            "HREFLANG_NOINDEX_TARGET",
            AddOpts {
                target_url: Some(source.clone()),
                occurrences_count: Some(count),
                details,
                ..Default::default()
            },
        )?;
    }
    let coverage_incomplete = ctx
        .native_hreflang
        .as_ref()
        .and_then(|n| {
            n.get("coverage")
                .and_then(|c| c.get("state"))
                .and_then(Value::as_str)
        })
        .map(|state| state != "complete")
        .unwrap_or(false);
    if by_source.is_empty() && (unmeasured > 0 || coverage_incomplete) {
        ctx.skip(
            "HREFLANG_NOINDEX_TARGET",
            "one or more hreflang targets or the retained population were unmeasured",
        );
    }
    Ok(())
}

/// HREFLANG_INCONSISTENT_CONFIRMATION — A declares B as "fr"; B says it is
/// "de".
///
/// Reciprocity asks only whether a return link exists. A pair can be fully
/// reciprocal and still be discarded, because the contract is that both sides
/// name the *same* language and region code: a page's counterparts must call
/// it what it calls itself. That is the fact this reads.
///
/// The counterpart's own self-referencing hreflang is the authority for what
/// it is, which is why a counterpart with no self-reference is passed over
/// entirely rather than guessed at — that page has no statement to disagree
/// with, and HREFLANG_MISSING_SELF_REFERENCE already names it. "x-default" is
/// excluded on both sides, and a declaration whose target was never crawled is
/// left alone for the same reason reciprocity leaves it alone: a page nobody
/// fetched cannot be faulted for what it does or does not say.
///
/// Reported against the *declaring* page, because that is where the annotation
/// that disagrees was written; the details name the counterpart, the code this
/// page claimed for it, and the codes the counterpart confirms for itself, so
/// the reader can see which of the two is wrong without opening both.
pub fn check_hreflang_confirmation_consistency(ctx: &mut AuditContext) -> Result<(), ContextError> {
    const XDEFAULT: &str = "x-default";
    if let Some(native) = native_snapshot(ctx) {
        // `confirmed_for`: the saved corpus is the authority when one is
        // attached; otherwise the declarations' own self-references stand in.
        let corpus_backed = ctx.saved_corpus.is_some();
        let mut derived: HashMap<String, BTreeSet<String>> = HashMap::new();
        if !corpus_backed {
            for item in &native.declarations {
                let code = fold_code(&item.get("lang").cloned().unwrap_or(Value::Null));
                if item
                    .get("source_representation_state")
                    .and_then(Value::as_str)
                    == Some("active")
                    && item.get("source_identity").and_then(Value::as_str)
                        == item.get("target_identity").and_then(Value::as_str)
                    && !code.is_empty()
                    && code != XDEFAULT
                {
                    if let Some(identity) = item.get("source_identity").and_then(Value::as_str) {
                        derived.entry(identity.to_owned()).or_default().insert(code);
                    }
                }
            }
        }
        for (source, rows) in native_source_groups(&native.declarations) {
            let mut mismatched: Vec<Value> = Vec::new();
            for item in &rows {
                let code = fold_code(&item.get("lang").cloned().unwrap_or(Value::Null));
                let source_identity = item.get("source_identity").and_then(Value::as_str);
                let target_identity = item.get("target_identity").and_then(Value::as_str);
                if code.is_empty() || code == XDEFAULT || source_identity == target_identity {
                    continue;
                }
                let Some(target_identity) = target_identity else {
                    continue;
                };
                let confirmed: BTreeSet<String> = if corpus_backed {
                    ctx.saved_corpus
                        .as_ref()
                        .map(|corpus| corpus.self_codes(target_identity))
                        .unwrap_or_default()
                } else {
                    derived.get(target_identity).cloned().unwrap_or_default()
                };
                let target = ctx.page_by_norm(target_identity);
                if !confirmed.is_empty() && !confirmed.contains(&code) {
                    if let Some(target) = target {
                        mismatched.push(json!({
                            "counterpart": target.url(),
                            "declared_here": code,
                            "confirmed_there": confirmed.iter().cloned().map(Value::from).collect::<Vec<_>>(),
                        }));
                    }
                }
            }
            if !mismatched.is_empty() {
                let count = mismatched.len() as i64;
                let page = ctx.page_by_norm(&norm_url(Some(&source))?);
                let target_url = page.map(|p| p.url()).unwrap_or_else(|| source.clone());
                let mut details = Object::new();
                details.insert("inconsistent".into(), Value::Array(mismatched));
                ctx.add(
                    "HREFLANG_INCONSISTENT_CONFIRMATION",
                    AddOpts {
                        target_url: Some(target_url),
                        occurrences_count: Some(count),
                        details,
                        ..Default::default()
                    },
                )?;
            }
        }
        return Ok(());
    }

    let records = {
        let Some(df) = ctx.exports.get("all_hreflang").filter(|df| !df.is_empty()) else {
            let reason = hreflang_unavailable(
                ctx,
                "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)",
            );
            ctx.skip("HREFLANG_INCONSISTENT_CONFIRMATION", &reason);
            return Ok(());
        };
        records_from_df(df, HREFLANG_FIELD_MAP)
    };
    // What each page declares about itself, and what every other page declares
    // about it, keyed the same way so the two are comparable at all.
    let mut self_codes: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut claims: IndexMap<String, Vec<(String, String)>> = IndexMap::new();
    // The URL as the export wrote it, for the case where neither end is in the
    // crawl's own page table: a normalized key is a comparison aid, not an
    // address a reader can open.
    let mut written_as: HashMap<String, String> = HashMap::new();
    for item in &records {
        let (Some(src), Some(dest)) = (
            item.get("source_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            item.get("destination_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let code = fold_code(&item.get("hreflang").cloned().unwrap_or(Value::Null));
        if code.is_empty() || code == XDEFAULT {
            continue;
        }
        let src_norm = norm_url(Some(src))?;
        let dest_norm = norm_url(Some(dest))?;
        written_as
            .entry(src_norm.clone())
            .or_insert_with(|| src.to_owned());
        written_as
            .entry(dest_norm.clone())
            .or_insert_with(|| dest.to_owned());
        if src_norm == dest_norm {
            self_codes.entry(src_norm).or_default().insert(code);
        } else {
            claims.entry(src_norm).or_default().push((dest_norm, code));
        }
    }
    let evidence = export_evidence(ctx, "all_hreflang");
    let claim_list: Vec<(String, Vec<(String, String)>)> = claims.into_iter().collect();
    for (src_norm, declared) in &claim_list {
        let source_page = ctx.page_by_norm(src_norm);
        let mut mismatched: Vec<Value> = Vec::new();
        for (dest_norm, code) in declared {
            let Some(confirmed) = self_codes.get(dest_norm) else {
                continue;
            };
            if confirmed.is_empty() || confirmed.contains(code) {
                continue; // no self-statement to contradict, or the two agree
            }
            let Some(target) = ctx.page_by_norm(dest_norm) else {
                continue; // counterpart was not crawled, so its self-row is not usable evidence
            };
            mismatched.push(json!({
                "counterpart": target.url(),
                "declared_here": code,
                "confirmed_there": confirmed.iter().cloned().map(Value::from).collect::<Vec<_>>(),
            }));
        }
        if !mismatched.is_empty() {
            let count = mismatched.len() as i64;
            let target_url = source_page
                .map(|p| p.url())
                .unwrap_or_else(|| written_as.get(src_norm).cloned().unwrap_or_default());
            let mut details = Object::new();
            details.insert("inconsistent".into(), Value::Array(mismatched));
            ctx.add(
                "HREFLANG_INCONSISTENT_CONFIRMATION",
                AddOpts {
                    target_url: Some(target_url),
                    occurrences_count: Some(count),
                    details,
                    evidence: evidence.clone(),
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}
