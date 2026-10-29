//! Reading the remediation ledger: summaries, case pages and reports.
//!
//! Every function here opens the ledger read-only (unless the caller already holds a connection)
//! and returns plain JSON-serializable views: the identity and count summary
//! ([`ledger_summary`]), the explicit remediation denominators ([`remediation_summary`]), the
//! paginated case and group history ([`read_cases`], [`read_group_members`]) and the
//! deterministic JSON/Markdown remediation report writers. Nothing here mutates the artifact.

use std::collections::BTreeMap;

use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Map, Value};

use super::ingest::{as_int, Record};
use super::schema::{canonical_url, ledger_error, LIFECYCLE_STATES, MAX_PAYLOAD_BYTES};
use super::LedgerRef;
use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::loads;

/// Cases per page when the caller names no limit.
pub const DEFAULT_CASE_LIMIT: i64 = 100;
/// Largest case page.
pub const MAX_CASE_LIMIT: i64 = 1000;

pub(crate) fn get(map: &Record, key: &str) -> Value {
    map.get(key).cloned().unwrap_or(Value::Null)
}

pub(crate) fn int(map: &Record, key: &str) -> i64 {
    map.get(key).and_then(as_int).unwrap_or(0)
}

/// Round a float to one decimal place, half to even.
pub(crate) fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

pub(crate) fn percent(numerator: i64, denominator: i64) -> Value {
    if denominator == 0 {
        Value::Null
    } else {
        json!(round1(100.0 * numerator as f64 / denominator as f64))
    }
}

pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn text_of(value: &Value) -> String {
    if super::ingest::truthy(value) {
        py_str(value)
    } else {
        String::new()
    }
}

/// Describe the boundary between represented cases and source group scope.
///
/// A group count can name a much larger population than the URLs retained in its issue rows.
/// Those missing members have no occurrence keys and cannot be assigned an outcome. The scope is
/// therefore evidence, not a guessed denominator: any truncated or unavailable group withholds
/// full-scope percentages while preserving exact metrics for represented occurrences.
pub fn membership_scope(
    con: &Connection,
    represented_occurrences: i64,
) -> Result<Value, StoreError> {
    let mut known_unrepresented: i64 = 0;
    let mut unknown_groups: i64 = 0;
    let mut incomplete_groups: i64 = 0;
    let mut samples: Vec<Value> = Vec::new();
    let rows = query_maps(
        con,
        "SELECT fg.source_scan_id,fg.group_ref,MAX(fg.group_count) AS group_count,COUNT(DISTINCT o.occurrence_id) AS represented FROM finding_group fg LEFT JOIN occurrence o ON o.finding_id=fg.finding_id GROUP BY fg.source_scan_id,fg.group_ref ORDER BY fg.source_scan_id,fg.group_ref",
        [],
    )?;
    for row in rows {
        let count = row.get("group_count").and_then(as_int);
        let represented = int(&row, "represented");
        let (kind, missing) = match count {
            Some(c) if c >= represented => {
                if c > represented {
                    let missing = c - represented;
                    known_unrepresented += missing;
                    incomplete_groups += 1;
                    ("partial", Some(missing))
                } else {
                    continue;
                }
            }
            _ => {
                unknown_groups += 1;
                incomplete_groups += 1;
                ("unknown", None)
            }
        };
        if samples.len() < 20 {
            samples.push(json!({
                "source_scan_id": int(&row, "source_scan_id"),
                "group_ref": get(&row, "group_ref"),
                "state": kind,
                "represented_members": represented,
                "known_unrepresented_members": missing,
            }));
        }
    }
    let source_states: Vec<String> = {
        let mut stmt = con.prepare(
            "SELECT group_memberships_state FROM source_scan WHERE group_memberships_state!='complete'",
        )?;
        let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
        mapped.collect::<Result<_, _>>()?
    };
    incomplete_groups += source_states.len() as i64;
    let state = if unknown_groups > 0 || source_states.iter().any(|s| s == "unavailable") {
        "unknown"
    } else if incomplete_groups > 0 {
        "partial"
    } else {
        "complete"
    };
    Ok(json!({
        "state": state,
        "represented_occurrences": represented_occurrences,
        "known_unrepresented_members": known_unrepresented,
        "unknown_group_memberships": unknown_groups + source_states.len() as i64,
        "incomplete_group_memberships": incomplete_groups,
        "samples": samples,
    }))
}

pub(crate) fn state_counts() -> BTreeMap<String, i64> {
    LIFECYCLE_STATES
        .iter()
        .map(|s| (s.to_string(), 0))
        .collect()
}

pub(crate) fn counts_value(counts: &BTreeMap<String, i64>) -> Value {
    Value::Object(counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

fn task_view(counts: &BTreeMap<String, i64>, scope_complete: bool, scope_state: &str) -> Value {
    let original: i64 = counts.values().sum();
    let actionable = original - counts["false_positive_reviewed"];
    let rechecked = counts["resolved"] + counts["persisting"] + counts["regressed"];
    let mut status = if original == 0 {
        "unavailable"
    } else if counts["unverifiable"] > 0 {
        "blocked"
    } else if ["detected", "verified", "fix_reported", "recheck_pending"]
        .iter()
        .any(|s| counts[*s] > 0)
    {
        "in_progress"
    } else {
        "complete"
    };
    if status == "complete" && !scope_complete {
        status = "scope_incomplete";
    }
    json!({
        "status": status,
        "counts": counts_value(counts),
        "denominators": {
            "verified_original_occurrences": original,
            "remediation_cases": actionable,
        },
        "represented_resolved_percent": percent(counts["resolved"], actionable),
        "represented_rechecked_percent": percent(rechecked, original),
        "scope_state": scope_state,
        "resolved_percent": if scope_complete { percent(counts["resolved"], actionable) } else { Value::Null },
        "rechecked_percent": if scope_complete { percent(rechecked, original) } else { Value::Null },
    })
}

/// Case counts and explicitly named remediation/recheck denominators.
///
/// `resolved_percent` includes unresolved evidence states in its denominator;
/// `rechecked_percent` deliberately excludes `unverifiable` from its numerator. This makes a
/// partial or failed recheck visible instead of shrinking the original case population.
pub fn remediation_summary(ledger: LedgerRef<'_>) -> Result<Record, StoreError> {
    let con = ledger.open(false)?;
    let mut counts = state_counts();
    {
        let mut stmt = con.prepare(
            "SELECT current_state,COUNT(*) AS count FROM occurrence GROUP BY current_state",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (state, n) = row?;
            counts.insert(state, n);
        }
    }
    let original: i64 = counts.values().sum();
    let remediation_denominator = original - counts["false_positive_reviewed"];
    let resolved = counts["resolved"];
    let rechecked = resolved + counts["persisting"] + counts["regressed"];
    let scope = membership_scope(&con, original)?;
    let scope_state = scope["state"].as_str().unwrap_or("unknown").to_string();
    let scope_complete = scope_state == "complete";
    let represented_resolved = percent(resolved, remediation_denominator);
    let represented_rechecked = percent(rechecked, original);
    let mut task_counts: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    {
        let mut stmt = con.prepare(
            "SELECT o.current_state,COALESCE((SELECT va.task_id FROM decision d JOIN verification_result vr ON vr.verification_result_id=d.verification_result_id JOIN verification_artifact va ON va.verification_id=vr.verification_id WHERE d.occurrence_id=o.occurrence_id ORDER BY d.decision_id DESC LIMIT 1),'unassigned') AS task_id FROM occurrence o",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (state, task) = row?;
            *task_counts
                .entry(task)
                .or_insert_with(state_counts)
                .entry(state)
                .or_insert(0) += 1;
        }
    }
    let revision: i64 = con.query_row(
        "SELECT ledger_revision FROM ledger WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let tasks: Map<String, Value> = task_counts
        .iter()
        .map(|(name, c)| (name.clone(), task_view(c, scope_complete, &scope_state)))
        .collect();
    let mut out = Record::new();
    out.insert("ok".into(), json!(true));
    out.insert("ledger_revision".into(), json!(revision));
    out.insert("counts".into(), counts_value(&counts));
    out.insert(
        "denominators".into(),
        json!({"verified_original_occurrences": original, "remediation_cases": remediation_denominator}),
    );
    out.insert("scope".into(), scope);
    out.insert(
        "represented_resolved_percent".into(),
        represented_resolved.clone(),
    );
    out.insert(
        "represented_rechecked_percent".into(),
        represented_rechecked.clone(),
    );
    out.insert(
        "resolved_percent".into(),
        if scope_complete {
            represented_resolved
        } else {
            Value::Null
        },
    );
    out.insert(
        "rechecked_percent".into(),
        if scope_complete {
            represented_rechecked
        } else {
            Value::Null
        },
    );
    out.insert("tasks".into(), Value::Object(tasks));
    out.insert(
        "reason".into(),
        if original > 0 {
            Value::Null
        } else {
            json!("the ledger has no occurrence population")
        },
    );
    Ok(out)
}

/// Ledger identity plus the independent finding/occurrence/URL counts.
pub fn ledger_summary(ledger: LedgerRef<'_>) -> Result<Record, StoreError> {
    let con = ledger.open(false)?;
    let mut counts = Map::new();
    for table in [
        "site",
        "source_scan",
        "check_def",
        "finding",
        "occurrence",
        "affected_url",
        "finding_observation",
        "finding_group",
        "source_group",
        "source_group_member",
        "observation",
        "decision",
        "verification_artifact",
        "verification_result",
    ] {
        let n: i64 = con.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
            r.get(0)
        })?;
        counts.insert(table.to_string(), json!(n));
    }
    let distinct: i64 = con.query_row("SELECT COUNT(DISTINCT url) FROM affected_url", [], |r| {
        r.get(0)
    })?;
    counts.insert("distinct_affected_urls".into(), json!(distinct));
    let header = query_maps(&con, "SELECT * FROM ledger WHERE singleton=1", [])?
        .into_iter()
        .next()
        .unwrap_or_default();
    let sites = query_maps(&con, "SELECT * FROM site ORDER BY site_id", [])?;
    let mut out = Record::new();
    out.insert("ok".into(), json!(true));
    out.insert("format_version".into(), get(&header, "format_version"));
    out.insert("ledger_uuid".into(), get(&header, "ledger_uuid"));
    out.insert("ledger_revision".into(), get(&header, "ledger_revision"));
    out.insert(
        "sites".into(),
        Value::Array(sites.into_iter().map(Value::Object).collect()),
    );
    out.insert("counts".into(), Value::Object(counts));
    Ok(out)
}

/// View of one observation; `previous` is the preceding view of the same occurrence.
pub fn observation_view(row: &Record, previous: Option<&Value>) -> Value {
    let basis = json!({
        "config_fingerprint": get(row, "config_fingerprint"),
        "producer_version": get(row, "producer_version"),
        "producer_revision": get(row, "producer_revision"),
    });
    let changed = match previous {
        None => Value::Null,
        Some(prev) => {
            let mut m = Map::new();
            for name in [
                "config_fingerprint",
                "producer_version",
                "producer_revision",
            ] {
                m.insert(name.into(), json!(basis[name] != prev["basis"][name]));
            }
            Value::Object(m)
        }
    };
    let flag = |k: &str| json!(super::ingest::truthy(&get(row, k)));
    let evidence = row
        .get("evidence_json")
        .and_then(Value::as_str)
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or(Value::Null);
    json!({
        "observation_id": get(row, "observation_id"),
        "observation_revision": get(row, "observation_revision"),
        "source_scan": {
            "source_scan_id": get(row, "source_scan_id"),
            "scan_uuid": get(row, "scan_uuid"),
            "format_version": get(row, "format_version"),
            "evidence_revision": get(row, "evidence_revision"),
            "audit_sha256": get(row, "audit_sha256"),
            "artifact_state": get(row, "artifact_state"),
            "crawl_partial": flag("crawl_partial"),
            "corpus_partial": flag("corpus_partial"),
        },
        "role": get(row, "role"),
        "issue_ordinal": get(row, "issue_ordinal"),
        "evidence_state": get(row, "evidence_state"),
        "evidence_reason": get(row, "evidence_reason"),
        "evidence": evidence,
        "payload_sha256": get(row, "payload_sha256"),
        "basis": basis,
        // Comparability is explicit data, not an inference: a changed basis marks the
        // observation as measured under different rules, never as resolved or overwritten.
        "basis_changed": changed,
        "observed_at": get(row, "observed_at"),
        "observed_at_state": get(row, "observed_at_state"),
        "ingested_at": get(row, "ingested_at"),
    })
}

/// An explicit bounded page from the complete retained source group.
pub fn read_group_members(
    ledger: LedgerRef<'_>,
    source_scan_id: i64,
    group_ref: &str,
    offset: i64,
    limit: i64,
    max_bytes: i64,
) -> Result<Record, StoreError> {
    if source_scan_id < 1 || group_ref.is_empty() {
        return Err(ledger_error(
            "group members require a positive source_scan_id and group_ref",
        ));
    }
    if offset < 0 || !(1..=MAX_CASE_LIMIT).contains(&limit) {
        return Err(ledger_error("group member offset or limit is invalid"));
    }
    if !(1..=MAX_PAYLOAD_BYTES).contains(&max_bytes) {
        return Err(ledger_error(
            "group member max_bytes must be between 1 and 8 MiB",
        ));
    }
    let con = ledger.open(false)?;
    let group = query_maps(
        &con,
        "SELECT * FROM source_group WHERE source_scan_id=? AND group_ref=?",
        params![source_scan_id, group_ref],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| ledger_error("source group membership is unavailable"))?;
    let mut rows: Vec<Value> = Vec::new();
    let mut used: i64 = 2;
    {
        let mut stmt = con.prepare(
            "SELECT value_json FROM source_group_member WHERE source_scan_id=? AND group_ref=? AND ordinal>=? ORDER BY ordinal LIMIT ?",
        )?;
        let mut cursor = stmt.query(params![source_scan_id, group_ref, offset, limit])?;
        while let Some(row) = cursor.next()? {
            let text: String = row.get(0)?;
            let size = text.len() as i64 + i64::from(!rows.is_empty());
            if used + size > max_bytes {
                if rows.is_empty() {
                    return Err(ledger_error(
                        "one group member exceeds the requested page byte bound",
                    ));
                }
                break;
            }
            rows.push(loads(&text, "group member")?);
            used += size;
        }
    }
    let total = int(&group, "member_count");
    let has_more = offset + (rows.len() as i64) < total;
    let returned = rows.len() as i64;
    let mut out = Record::new();
    out.insert("source_scan_id".into(), json!(source_scan_id));
    out.insert("group_ref".into(), json!(group_ref));
    out.insert("rows".into(), Value::Array(rows));
    out.insert("total".into(), json!(total));
    out.insert("offset".into(), json!(offset));
    out.insert("limit".into(), json!(limit));
    out.insert("returned".into(), json!(returned));
    out.insert("has_more".into(), json!(has_more));
    out.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + returned)
        } else {
            Value::Null
        },
    );
    out.insert("bytes".into(), json!(used));
    out.insert("max_bytes".into(), json!(max_bytes));
    out.insert("members_sha256".into(), get(&group, "members_sha256"));
    Ok(out)
}

/// Exact filters of [`read_cases`].
#[derive(Default, Clone)]
pub struct CaseFilter<'a> {
    /// Registry check id.
    pub check: Option<&'a str>,
    /// URL, canonicalized before matching subjects or affected-URL membership.
    pub url: Option<&'a str>,
    /// One finding directly.
    pub finding_key: Option<&'a str>,
}

/// Read back exact cases and their per-occurrence history.
///
/// Filters are exact: `check` is a registry check id, `url` is canonicalized with the audit's
/// own URL policy before matching subjects or affected-URL membership, and `finding_key`
/// selects one case directly. The result is the ledger population itself: findings, their
/// occurrences, affected-URL membership, group memberships and ordered observations. Results are
/// paginated by finding to keep an observer responsive; pass `limit=None` only for a bounded
/// local export such as [`remediation_report`].
pub fn read_cases(
    ledger: LedgerRef<'_>,
    filter: &CaseFilter<'_>,
    limit: Option<i64>,
    offset: i64,
) -> Result<Record, StoreError> {
    if limit.is_some_and(|l| !(1..=MAX_CASE_LIMIT).contains(&l)) {
        return Err(ledger_error(format!(
            "limit must be an integer from 1 to {MAX_CASE_LIMIT} or None"
        )));
    }
    if !(0..=1_000_000).contains(&offset) {
        return Err(ledger_error("offset must be a nonnegative bounded integer"));
    }
    let con = ledger.open(false)?;
    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(check) = filter.check {
        clauses.push("c.check_key=?");
        values.push(check.to_string().into());
    }
    if let Some(key) = filter.finding_key {
        clauses.push("f.finding_key=?");
        values.push(key.to_string().into());
    }
    if let Some(url) = filter.url {
        let canon = canonical_url(url)?;
        clauses.push(
            "(f.subject_value=? OR EXISTS (SELECT 1 FROM affected_url a WHERE a.finding_id=f.finding_id AND a.url=?))",
        );
        values.push(canon.clone().into());
        values.push(canon.into());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let total: i64 = con.query_row(
        &format!(
            "SELECT COUNT(*) FROM finding f JOIN check_def c ON c.check_id=f.check_id {where_sql}"
        ),
        params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    let mut page = String::new();
    let mut page_values = values.clone();
    if let Some(l) = limit {
        page = " LIMIT ? OFFSET ?".into();
        page_values.push(l.into());
        page_values.push(offset.into());
    }
    let finding_rows = query_maps(
        &con,
        &format!(
            "SELECT f.*,c.check_key FROM finding f JOIN check_def c ON c.check_id=f.check_id {where_sql} ORDER BY c.check_key,f.subject_value{page}"
        ),
        params_from_iter(page_values.iter()),
    )?;
    let mut findings = Vec::new();
    for finding in finding_rows {
        findings.push(read_finding(&con, &finding)?);
    }
    let revision: i64 = con.query_row(
        "SELECT ledger_revision FROM ledger WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let mut out = Record::new();
    out.insert("ok".into(), json!(true));
    out.insert("ledger_revision".into(), json!(revision));
    out.insert("total".into(), json!(total));
    out.insert("limit".into(), limit.map_or(Value::Null, |l| json!(l)));
    out.insert("offset".into(), json!(offset));
    out.insert("findings".into(), Value::Array(findings));
    Ok(out)
}

fn read_finding(con: &Connection, finding: &Record) -> Result<Value, StoreError> {
    let finding_id = int(finding, "finding_id");
    let members: Vec<Value> = query_maps(
        con,
        "SELECT * FROM affected_url WHERE finding_id=? ORDER BY url,role",
        [finding_id],
    )?
    .iter()
    .map(|row| json!({"url": get(row, "url"), "role": get(row, "role"), "first_seen_at": get(row, "first_seen_at")}))
    .collect();
    let projections: Vec<Value> = query_maps(
        con,
        "SELECT * FROM finding_observation WHERE finding_id=? ORDER BY source_scan_id",
        [finding_id],
    )?
    .iter()
    .map(|row| {
        json!({
            "source_scan_id": get(row, "source_scan_id"),
            "issue_ordinal": get(row, "issue_ordinal"),
            "audit_fingerprint": get(row, "audit_fingerprint"),
            "severity": get(row, "severity"),
            "status_code": get(row, "status_code"),
            "message": get(row, "message"),
            "occurrences_count": get(row, "occurrences_count"),
            "enumerated_count": get(row, "enumerated_count"),
            "coverage_state": get(row, "coverage_state"),
            "payload_sha256": get(row, "payload_sha256"),
        })
    })
    .collect();
    let mut memberships = Vec::new();
    for row in query_maps(
        con,
        "SELECT * FROM finding_group WHERE finding_id=? ORDER BY source_scan_id",
        [finding_id],
    )? {
        let scan_id = int(&row, "source_scan_id");
        let group_ref = row
            .get("group_ref")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let present: bool = con
            .prepare("SELECT 1 FROM source_group WHERE source_scan_id=? AND group_ref=?")?
            .exists(params![scan_id, group_ref])?;
        let group_page = if present {
            Some(read_group_members(
                LedgerRef::Conn(con),
                scan_id,
                &group_ref,
                0,
                25,
                1024 * 1024,
            )?)
        } else {
            None
        };
        let (urls, page_meta) = match group_page {
            Some(mut page) => {
                let rows = page.remove("rows").unwrap_or(Value::Array(vec![]));
                (rows, Value::Object(page))
            }
            None => (json!([]), json!({"state": "unavailable"})),
        };
        memberships.push(json!({
            "source_scan_id": scan_id,
            "group_ref": group_ref,
            "group_check": get(&row, "group_check"),
            "group_value": get(&row, "group_value"),
            "group_count": get(&row, "group_count"),
            "group_urls": urls,
            "group_members_page": page_meta,
        }));
    }
    let mut occurrences = Vec::new();
    for occurrence in query_maps(
        con,
        "SELECT * FROM occurrence WHERE finding_id=? ORDER BY subject_value,representation,discriminator_value",
        [finding_id],
    )? {
        let occurrence_id = int(&occurrence, "occurrence_id");
        let mut observations: Vec<Value> = Vec::new();
        for row in query_maps(
            con,
            "SELECT o.*,s.scan_uuid,s.format_version,s.evidence_revision,s.audit_sha256,s.artifact_state,s.crawl_partial,s.corpus_partial FROM observation o JOIN source_scan s ON s.source_scan_id=o.source_scan_id WHERE o.occurrence_id=? ORDER BY o.observation_revision",
            [occurrence_id],
        )? {
            let view = observation_view(&row, observations.last());
            observations.push(view);
        }
        let mut decisions = Vec::new();
        for row in query_maps(
            con,
            "SELECT d.*,vr.outcome AS verification_outcome,vr.measured AS verification_measured,vr.before_sha256,vr.after_sha256,vr.scope_sha256,va.artifact_sha256,va.artifact_path,va.task_id,va.baseline_audit_sha256,va.selection_sha256,va.collection_sha256,va.collection_state FROM decision d LEFT JOIN verification_result vr ON vr.verification_result_id=d.verification_result_id LEFT JOIN verification_artifact va ON va.verification_id=vr.verification_id WHERE d.occurrence_id=? ORDER BY d.decision_id",
            [occurrence_id],
        )? {
            let verification = if get(&row, "verification_result_id").is_null() {
                Value::Null
            } else {
                json!({
                    "artifact_sha256": get(&row, "artifact_sha256"),
                    "artifact_path": get(&row, "artifact_path"),
                    "task_id": get(&row, "task_id"),
                    "baseline_audit_sha256": get(&row, "baseline_audit_sha256"),
                    "selection_sha256": get(&row, "selection_sha256"),
                    "collection_sha256": get(&row, "collection_sha256"),
                    "collection_state": get(&row, "collection_state"),
                    "outcome": get(&row, "verification_outcome"),
                    "measured": super::ingest::truthy(&get(&row, "verification_measured")),
                    "before_sha256": get(&row, "before_sha256"),
                    "after_sha256": get(&row, "after_sha256"),
                    "scope_sha256": get(&row, "scope_sha256"),
                })
            };
            decisions.push(json!({
                "decision_id": get(&row, "decision_id"),
                "state": get(&row, "state"),
                "actor": get(&row, "actor"),
                "reason": get(&row, "reason"),
                "decided_at": get(&row, "decided_at"),
                "decided_at_state": get(&row, "decided_at_state"),
                "observation_id": get(&row, "observation_id"),
                "ledger_revision": get(&row, "ledger_revision"),
                "verification": verification,
            }));
        }
        occurrences.push(json!({
            "occurrence_id": get(&occurrence, "occurrence_id"),
            "occurrence_key": get(&occurrence, "occurrence_key"),
            "subject_type": get(&occurrence, "subject_type"),
            "subject_value": get(&occurrence, "subject_value"),
            "representation": get(&occurrence, "representation"),
            "discriminator_type": get(&occurrence, "discriminator_type"),
            "discriminator_value": get(&occurrence, "discriminator_value"),
            "current_state": get(&occurrence, "current_state"),
            "first_seen_at": get(&occurrence, "first_seen_at"),
            "observations": observations,
            "decisions": decisions,
        }));
    }
    Ok(json!({
        "finding_id": get(finding, "finding_id"),
        "finding_key": get(finding, "finding_key"),
        "check": get(finding, "check_key"),
        "subject_type": get(finding, "subject_type"),
        "subject_value": get(finding, "subject_value"),
        "current_state": get(finding, "current_state"),
        "first_seen_at": get(finding, "first_seen_at"),
        "affected_urls": members,
        "projections": projections,
        "group_memberships": memberships,
        "occurrences": occurrences,
    }))
}

pub use super::reports::{remediation_markdown, remediation_report, write_remediation_report};
