//! Lifecycle decisions of the remediation ledger: transitions and verification results.
//!
//! `transition_occurrence` / `record_verification` half of the module.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::ingest::{as_int, json_sha256, validated_time, Record};
use super::schema::{
    canonical_url, is_fingerprint, is_sha256, ledger_error, transitions_from, LIFECYCLE_STATES,
    MAX_PAYLOAD_BYTES, MEASURED_OUTCOMES,
};
use super::LedgerRef;
use crate::error::StoreError;
use crate::scan_lease::utc;

/// Trimmed nonempty text of at most `maximum` characters, else a ledger error.
pub fn lifecycle_text(value: &Value, name: &str, maximum: i64) -> Result<String, StoreError> {
    match value.as_str() {
        Some(s) if !s.trim().is_empty() && s.chars().count() as i64 <= maximum => {
            Ok(s.trim().to_string())
        }
        _ => Err(ledger_error(format!(
            "{name} must be nonempty text of at most {maximum} characters"
        ))),
    }
}

/// Whether one occurrence belongs to an equivalent retained baseline.
///
/// A scan UUID remains provenance only: it is reusable in copied or altered documents. Legacy
/// audit JSON can use the canonical digest retained at ingestion, while audit.v2 uses its
/// independently validated exact digest. Both routes require the originally recorded
/// results-affecting policy.
pub fn occurrence_matches_baseline(
    con: &Connection,
    occurrence_id: i64,
    audit_sha256: &str,
    results_policy_fingerprint: &str,
) -> Result<bool, StoreError> {
    if occurrence_id < 1 {
        return Err(ledger_error("occurrence_id must be a positive integer"));
    }
    if !is_sha256(audit_sha256) {
        return Err(ledger_error("baseline audit_sha256 is invalid"));
    }
    if !is_fingerprint(results_policy_fingerprint) {
        return Err(ledger_error(
            "baseline results_policy_fingerprint is invalid",
        ));
    }
    let row: Option<i64> = con
        .query_row(
            "SELECT 1 FROM observation ob JOIN source_scan s ON s.source_scan_id=ob.source_scan_id WHERE ob.occurrence_id=? AND ob.role='target' AND (s.audit_sha256=? OR s.canonical_audit_sha256=?) AND s.config_fingerprint=? LIMIT 1",
            params![occurrence_id, audit_sha256, audit_sha256, results_policy_fingerprint],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.is_some())
}

/// Project exact occurrence states to one conservative finding state.
///
/// A finding is resolved only when every retained occurrence is resolved. Mixed evidence is never
/// flattened to resolved; the most actionable retained state remains visible to a page that
/// lists findings.
pub fn finding_state(con: &Connection, finding_id: i64) -> Result<String, StoreError> {
    let mut stmt = con.prepare("SELECT current_state FROM occurrence WHERE finding_id=?")?;
    let rows = stmt.query_map([finding_id], |r| r.get::<_, String>(0))?;
    let states: BTreeSet<String> = rows.collect::<Result<_, _>>()?;
    let has = |s: &str| states.contains(s);
    let only = |s: &str| states.len() == 1 && states.contains(s);
    let state = if states.is_empty() {
        "detected"
    } else if only("resolved") {
        "resolved"
    } else if has("regressed") {
        "regressed"
    } else if has("persisting") {
        "persisting"
    } else if has("recheck_pending") {
        "recheck_pending"
    } else if only("false_positive_reviewed") {
        "false_positive_reviewed"
    } else if has("unverifiable") {
        "unverifiable"
    } else if has("fix_reported") {
        "fix_reported"
    } else if has("verified") {
        "verified"
    } else {
        "detected"
    };
    Ok(state.to_string())
}

/// Validate an evidence reference used for a measured lifecycle outcome.
pub fn outcome_observation(
    con: &Connection,
    occurrence_id: i64,
    observation_id: Option<i64>,
) -> Result<i64, StoreError> {
    let Some(observation_id) = observation_id else {
        return Err(ledger_error(
            "a measured lifecycle outcome requires an observation_id",
        ));
    };
    if observation_id < 1 {
        return Err(ledger_error("observation_id must be a positive integer"));
    }
    let row: Option<(i64, i64, String, String)> = con
        .query_row(
            "SELECT o.observation_id,o.observation_revision,o.evidence_state,s.artifact_state FROM observation o JOIN source_scan s ON s.source_scan_id=o.source_scan_id WHERE o.observation_id=? AND o.occurrence_id=?",
            params![observation_id, occurrence_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, revision, evidence_state, artifact_state)) = row else {
        return Err(ledger_error(
            "observation_id does not belong to this occurrence",
        ));
    };
    if evidence_state != "measured" || artifact_state != "present" {
        return Err(ledger_error(
            "measured lifecycle outcomes require retained measured evidence",
        ));
    }
    if revision <= 1 {
        return Err(ledger_error(
            "measured lifecycle outcomes require a later observation, not baseline evidence",
        ));
    }
    Ok(id)
}

/// Run `body` inside `BEGIN IMMEDIATE`, committing on success and rolling back on any error.
fn in_write_transaction<T>(
    con: &Connection,
    body: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    con.execute_batch("BEGIN IMMEDIATE")?;
    match body() {
        Ok(v) => {
            con.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Arguments of one lifecycle transition.
pub struct Transition<'a> {
    /// Lowercase SHA-256 occurrence key.
    pub occurrence_key: &'a str,
    /// Target lifecycle state.
    pub state: &'a str,
    /// Who decided.
    pub actor: &'a str,
    /// Why.
    pub reason: &'a str,
    /// The ledger revision the caller read.
    pub expected_revision: i64,
    /// Evidence reference for a measured outcome.
    pub observation_id: Option<i64>,
    /// UTC ISO-8601 decision time; the clock when absent.
    pub decided_at: Option<&'a str>,
}

/// Append one validated, revision-safe lifecycle decision for a case.
///
/// The caller supplies the ledger revision read from `ledger_summary` or `read_cases`. A stale
/// writer is refused before mutation. In particular, a user assertion cannot resolve a finding:
/// `resolved`, `persisting` and `regressed` require a retained measured observation after the
/// baseline.
pub fn transition_occurrence(
    ledger: LedgerRef<'_>,
    request: &Transition<'_>,
) -> Result<Record, StoreError> {
    if !is_sha256(request.occurrence_key) {
        return Err(ledger_error(
            "occurrence_key must be a lowercase SHA-256 key",
        ));
    }
    if !LIFECYCLE_STATES.contains(&request.state) {
        return Err(ledger_error("unsupported lifecycle state"));
    }
    let actor = lifecycle_text(&json!(request.actor), "actor", 128)?;
    let reason = lifecycle_text(&json!(request.reason), "reason", 2048)?;
    if request.expected_revision < 0 {
        return Err(ledger_error(
            "expected_revision must be a nonnegative integer",
        ));
    }
    if let Some(at) = request.decided_at {
        if validated_time(&json!(at)).is_none() {
            return Err(ledger_error("decided_at must be a UTC ISO-8601 timestamp"));
        }
    }
    let con = ledger.open(true)?;
    let state = request.state;
    in_write_transaction(&con, || {
        let header: Option<i64> = con
            .query_row(
                "SELECT ledger_revision FROM ledger WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if header != Some(request.expected_revision) {
            return Err(ledger_error(
                "ledger revision changed; reread cases before recording a decision",
            ));
        }
        let occurrence: Option<(i64, i64, String)> = con
            .query_row(
                "SELECT occurrence_id,finding_id,current_state FROM occurrence WHERE occurrence_key=?",
                [request.occurrence_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((occurrence_id, finding_id, previous)) = occurrence else {
            return Err(ledger_error("unknown occurrence_key"));
        };
        if state == previous {
            return Err(ledger_error(
                "lifecycle transition must change the current state",
            ));
        }
        if !transitions_from(&previous).contains(&state) {
            return Err(ledger_error(format!(
                "invalid lifecycle transition from '{previous}' to '{state}'"
            )));
        }
        let measured = MEASURED_OUTCOMES.contains(&state);
        let linked = if measured || request.observation_id.is_some() {
            Some(outcome_observation(
                &con,
                occurrence_id,
                request.observation_id,
            )?)
        } else {
            None
        };
        let next_revision = request.expected_revision + 1;
        let recorded_at = match request.decided_at {
            Some(at) => at.to_string(),
            None => utc()?,
        };
        con.execute(
            "INSERT INTO decision(occurrence_id,state,actor,reason,decided_at,decided_at_state,observation_id,ledger_revision) VALUES(?,?,?,?,?,?,?,?)",
            params![occurrence_id, state, actor, reason, recorded_at, "known", linked, next_revision],
        )?;
        con.execute(
            "UPDATE occurrence SET current_state=? WHERE occurrence_id=?",
            params![state, occurrence_id],
        )?;
        let finding = finding_state(&con, finding_id)?;
        con.execute(
            "UPDATE finding SET current_state=? WHERE finding_id=?",
            params![finding, finding_id],
        )?;
        con.execute(
            "UPDATE ledger SET ledger_revision=? WHERE singleton=1",
            [next_revision],
        )?;
        let mut out = Record::new();
        out.insert("ok".into(), json!(true));
        out.insert("occurrence_key".into(), json!(request.occurrence_key));
        out.insert("previous_state".into(), json!(previous));
        out.insert("state".into(), json!(state));
        out.insert("finding_state".into(), json!(finding));
        out.insert(
            "observation_id".into(),
            linked.map_or(Value::Null, |o| json!(o)),
        );
        out.insert("ledger_revision".into(), json!(next_revision));
        Ok(out)
    })
}

struct Prepared {
    occurrence_id: i64,
    finding_id: i64,
    state: &'static str,
    reason: String,
    row_index: usize,
    before_sha256: String,
    after_sha256: Option<String>,
    scope_sha256: String,
    measured: bool,
    detail: String,
}

fn outcome_for(status: &str) -> Option<&'static str> {
    match status {
        "resolved" => Some("resolved"),
        "persisting" => Some("persisting"),
        "changed" => Some("regressed"),
        "not_verifiable" => Some("unverifiable"),
        _ => None,
    }
}

/// Apply one retained `verification.v1` artifact to pending ledger cases.
///
/// The artifact remains a standalone immutable file, while the ledger stores its byte digest,
/// exact selected scope and one typed result per occurrence. Measured outcomes are refused
/// unless the guarded verifier retained the before/after page evidence that supports them.
/// `occurrence_keys` is the preferred exact binding used by the ledger-aware recheck command;
/// the legacy check/URL lookup remains only for one unambiguous local occurrence.
pub fn record_verification(
    ledger: LedgerRef<'_>,
    verification_path: &Path,
    actor: &str,
    expected_revision: i64,
    occurrence_keys: Option<&[String]>,
    task_id: &str,
) -> Result<Record, StoreError> {
    let meta = std::fs::symlink_metadata(verification_path).ok();
    if !meta
        .as_ref()
        .is_some_and(|m| m.is_file() && m.len() as i64 <= MAX_PAYLOAD_BYTES)
    {
        return Err(ledger_error(
            "verification artifact must be a bounded regular file",
        ));
    }
    // Read the artifact bytes once: the parsed document and the stored digest must cover
    // the same bytes, so the file cannot change between the read and the hash.
    let mut bytes = Vec::new();
    if std::fs::File::open(verification_path)
        .and_then(|file| {
            file.take(MAX_PAYLOAD_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .is_err()
    {
        return Err(ledger_error("verification artifact is not valid JSON"));
    }
    if bytes.len() as i64 > MAX_PAYLOAD_BYTES {
        return Err(ledger_error(
            "verification artifact must be a bounded regular file",
        ));
    }
    let document: Value = serde_json::from_slice(&bytes)
        .ok()
        .ok_or_else(|| ledger_error("verification artifact is not valid JSON"))?;
    let artifact_sha256: String = {
        let mut digest = Sha256::new();
        digest.update(&bytes);
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    if document.get("schema_version").and_then(Value::as_str) != Some("verification.v1") {
        return Err(ledger_error(
            "verification artifact must use schema_version verification.v1",
        ));
    }
    let findings = document
        .get("findings")
        .and_then(Value::as_array)
        .filter(|f| !f.is_empty())
        .ok_or_else(|| ledger_error("verification artifact has no finding results"))?;
    if expected_revision < 0 {
        return Err(ledger_error(
            "expected_revision must be a nonnegative integer",
        ));
    }
    let actor = lifecycle_text(&json!(actor), "actor", 128)?;
    let task_id = lifecycle_text(&json!(task_id), "task_id", 128)?;
    if let Some(keys) = occurrence_keys {
        if keys.len() != findings.len() {
            return Err(ledger_error(
                "occurrence_keys must name exactly one key for every verification row",
            ));
        }
        if keys.iter().any(|k| !is_sha256(k)) {
            return Err(ledger_error(
                "occurrence_keys must contain lowercase SHA-256 keys",
            ));
        }
        if keys.iter().collect::<BTreeSet<_>>().len() != keys.len() {
            return Err(ledger_error(
                "occurrence_keys must not repeat a ledger case",
            ));
        }
    }
    let object = |name: &str| document.get(name).filter(|v| v.is_object());
    let (Some(baseline), Some(selection), Some(collection)) = (
        object("baseline"),
        object("selection"),
        object("collection"),
    ) else {
        return Err(ledger_error(
            "verification artifact lacks baseline, selection or collection evidence",
        ));
    };
    let baseline_sha = baseline
        .get("audit_sha256")
        .and_then(Value::as_str)
        .filter(|s| is_sha256(s))
        .ok_or_else(|| ledger_error("verification artifact baseline audit_sha256 is invalid"))?
        .to_string();
    let baseline_policy = baseline
        .get("results_policy_fingerprint")
        .and_then(Value::as_str)
        .filter(|s| is_fingerprint(s))
        .ok_or_else(|| {
            ledger_error("verification artifact baseline results_policy_fingerprint is invalid")
        })?
        .to_string();
    if let Some(uuid) = baseline.get("scan_uuid").filter(|v| !v.is_null()) {
        if !uuid.as_str().is_some_and(|s| !s.is_empty()) {
            return Err(ledger_error(
                "verification artifact baseline scan_uuid is invalid",
            ));
        }
    }
    let collection_state = collection
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| {
            [
                "measured",
                "offline",
                "partial",
                "not_run",
                "not_verifiable",
            ]
            .contains(s)
        })
        .ok_or_else(|| ledger_error("verification artifact collection state is invalid"))?
        .to_string();
    let con = ledger.open(true)?;
    in_write_transaction(&con, || {
        let header: Option<i64> = con
            .query_row(
                "SELECT ledger_revision FROM ledger WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if header != Some(expected_revision) {
            return Err(ledger_error(
                "ledger revision changed; reread cases before recording verification",
            ));
        }
        let existing: Option<i64> = con
            .query_row(
                "SELECT verification_id FROM verification_artifact WHERE artifact_sha256=?",
                [&artifact_sha256],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(ledger_error("verification artifact was already recorded"));
        }
        let absolute = std::path::absolute(verification_path)?;
        con.execute(
            "INSERT INTO verification_artifact(artifact_sha256,schema_version,artifact_path,task_id,baseline_audit_sha256,selection_sha256,collection_sha256,collection_state,recorded_at) VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                artifact_sha256,
                "verification.v1",
                absolute.to_string_lossy(),
                task_id,
                baseline_sha,
                json_sha256(selection),
                json_sha256(collection),
                collection_state,
                utc()?
            ],
        )?;
        let verification_id = con.last_insert_rowid();
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut seen: BTreeSet<i64> = BTreeSet::new();
        for (index, item) in findings.iter().enumerate() {
            let Some(item) = item.as_object() else {
                return Err(ledger_error(
                    "verification finding result must be an object",
                ));
            };
            let check = item.get("check").and_then(Value::as_str);
            let url = item.get("url").and_then(Value::as_str);
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .and_then(|s| outcome_for(s).map(|_| s));
            let (Some(check), Some(url), Some(status)) = (check, url, status) else {
                return Err(ledger_error(
                    "verification finding has unsupported check, URL or status",
                ));
            };
            let canonical = canonical_url(url)?;
            let rows: Vec<(i64, i64, String)> = match occurrence_keys {
                None => {
                    let mut stmt = con.prepare(
                        "SELECT o.occurrence_id,o.finding_id,o.current_state FROM occurrence o JOIN check_def c ON c.check_id=o.check_id WHERE c.check_key=? AND o.subject_type='url' AND o.subject_value=?",
                    )?;
                    let mapped = stmt.query_map(params![check, canonical], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
                    mapped.collect::<Result<_, _>>()?
                }
                Some(keys) => {
                    let mut stmt = con.prepare(
                        "SELECT o.occurrence_id,o.finding_id,o.current_state FROM occurrence o JOIN check_def c ON c.check_id=o.check_id WHERE o.occurrence_key=? AND o.subject_type='url' AND o.subject_value=? AND c.check_key=?",
                    )?;
                    let mapped = stmt.query_map(params![keys[index], canonical, check], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
                    mapped.collect::<Result<_, _>>()?
                }
            };
            if rows.len() != 1 {
                return Err(ledger_error(
                    "verification finding does not map to exactly one ledger occurrence",
                ));
            }
            let (occurrence_id, finding_id, current) = &rows[0];
            if seen.contains(occurrence_id) {
                return Err(ledger_error(
                    "verification artifact maps multiple rows to one ledger occurrence",
                ));
            }
            if current != "recheck_pending" {
                return Err(ledger_error(
                    "verification result requires a recheck_pending occurrence",
                ));
            }
            if !occurrence_matches_baseline(&con, *occurrence_id, &baseline_sha, &baseline_policy)?
            {
                return Err(ledger_error(
                    "verification artifact baseline does not bind the selected ledger case",
                ));
            }
            let null = Value::Null;
            let before = item.get("before").unwrap_or(&null);
            let before_page = item.get("before_page").unwrap_or(&null);
            let after = item.get("after").unwrap_or(&null);
            let after_page = item.get("after_page").unwrap_or(&null);
            if !before.is_object() || !before_page.is_object() {
                return Err(ledger_error(
                    "verification result lacks exact baseline finding/page evidence",
                ));
            }
            let state = outcome_for(status).unwrap_or("unverifiable");
            let measured = MEASURED_OUTCOMES.contains(&state);
            if measured && !["measured", "offline", "partial"].contains(&collection_state.as_str())
            {
                return Err(ledger_error(
                    "measured verification outcome lacks a completed collection",
                ));
            }
            if measured && !after_page.is_object() {
                return Err(ledger_error(
                    "measured verification outcome lacks an after page observation",
                ));
            }
            let detail = item
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let reason = lifecycle_text(
                &json!(format!(
                    "verification.v1 sha256={artifact_sha256} row={index}; {}",
                    if detail.is_empty() {
                        "bounded recheck result"
                    } else {
                        &detail
                    }
                )),
                "reason",
                2048,
            )?;
            prepared.push(Prepared {
                occurrence_id: *occurrence_id,
                finding_id: *finding_id,
                state,
                reason,
                row_index: index,
                before_sha256: json_sha256(&json!({"finding": before, "page": before_page})),
                after_sha256: after_page
                    .is_object()
                    .then(|| json_sha256(&json!({"finding": after, "page": after_page}))),
                scope_sha256: json_sha256(&json!({
                    "check": check,
                    "url": canonical,
                    "before": {"finding": before, "page": before_page},
                    "after": {"finding": after, "page": after_page},
                })),
                measured,
                detail,
            });
            seen.insert(*occurrence_id);
        }
        let next_revision = expected_revision + 1;
        for p in &prepared {
            con.execute(
                "INSERT INTO decision(occurrence_id,state,actor,reason,decided_at,decided_at_state,observation_id,ledger_revision,verification_result_id) VALUES(?,?,?,?,?,?,?,?,?)",
                params![p.occurrence_id, p.state, actor, p.reason, utc()?, "known", Option::<i64>::None, next_revision, Option::<i64>::None],
            )?;
            let decision_id = con.last_insert_rowid();
            con.execute(
                "INSERT INTO verification_result(verification_id,occurrence_id,row_index,outcome,measured,before_sha256,after_sha256,scope_sha256,reason) VALUES(?,?,?,?,?,?,?,?,?)",
                params![
                    verification_id,
                    p.occurrence_id,
                    p.row_index as i64,
                    p.state,
                    i64::from(p.measured),
                    p.before_sha256,
                    p.after_sha256,
                    p.scope_sha256,
                    p.detail
                ],
            )?;
            let result_id = con.last_insert_rowid();
            con.execute(
                "UPDATE decision SET verification_result_id=? WHERE decision_id=?",
                params![result_id, decision_id],
            )?;
            con.execute(
                "UPDATE occurrence SET current_state=? WHERE occurrence_id=?",
                params![p.state, p.occurrence_id],
            )?;
            let finding = finding_state(&con, p.finding_id)?;
            con.execute(
                "UPDATE finding SET current_state=? WHERE finding_id=?",
                params![finding, p.finding_id],
            )?;
        }
        con.execute(
            "UPDATE ledger SET ledger_revision=? WHERE singleton=1",
            [next_revision],
        )?;
        let mut out = Record::new();
        out.insert("ok".into(), json!(true));
        out.insert("ledger_revision".into(), json!(next_revision));
        out.insert("verification_sha256".into(), json!(artifact_sha256));
        out.insert("verification_id".into(), json!(verification_id));
        out.insert("task_id".into(), json!(task_id));
        out.insert("recorded".into(), json!(prepared.len()));
        Ok(out)
    })
}

/// Silence the unused-import lint for helpers shared with the ingest module.
#[allow(dead_code)]
fn _shared(value: &Value) -> Option<i64> {
    as_int(value)
}
