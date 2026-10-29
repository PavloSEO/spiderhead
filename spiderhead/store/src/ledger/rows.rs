//! Row writers of the remediation ledger: checks, findings, occurrences, groups, observations.
//!
//! Each insert returns the existing row when the identical record is already stored.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::ingest::{as_int, py_str, truthy, Record};
use super::schema::{finding_key, ledger_error, occurrence_key, MAX_PAYLOAD_BYTES};
use super::specs::{OccurrenceSpec, REPRESENTATIONS};
use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::dump;

/// Resolve a check key to its row id, inserting it unless the source revision is already bound.
pub fn check_id(con: &Connection, check_key: &Value, refuse_new: bool) -> Result<i64, StoreError> {
    let Some(name) = check_key
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 128)
    else {
        return Err(ledger_error(
            "an audit issue requires a bounded check identifier",
        ));
    };
    let found: Option<i64> = con
        .query_row(
            "SELECT check_id FROM check_def WHERE check_key=?",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    if refuse_new {
        return Err(conflicting());
    }
    con.execute("INSERT INTO check_def(check_key) VALUES(?)", [name])?;
    Ok(con.last_insert_rowid())
}

fn conflicting() -> StoreError {
    ledger_error("conflicting evidence under a previously bound source revision")
}

/// Identity of one finding in a project/site.
pub struct FindingIdentity<'a> {
    /// Site row id.
    pub site_id: i64,
    /// Check row id.
    pub check_id: i64,
    /// Check key.
    pub check_key: &'a str,
    /// Project UUID of the site.
    pub project_uuid: &'a str,
    /// Site host.
    pub site_host: &'a str,
    /// `url` or `scope`.
    pub subject_type: &'a str,
    /// Canonical URL or the scope subject.
    pub subject_value: &'a str,
}

/// Insert (or find) a finding: `(finding_id, created, key)`.
pub fn insert_finding(
    con: &Connection,
    id: &FindingIdentity<'_>,
    now: &str,
    refuse_new: bool,
) -> Result<(i64, bool, String), StoreError> {
    let key = finding_key(
        id.project_uuid,
        id.site_host,
        id.check_key,
        id.subject_type,
        id.subject_value,
    );
    let found: Option<i64> = con
        .query_row(
            "SELECT finding_id FROM finding WHERE finding_key=?",
            [&key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(found) = found {
        return Ok((found, false, key));
    }
    if refuse_new {
        return Err(conflicting());
    }
    con.execute(
        "INSERT INTO finding(site_id,check_id,subject_type,subject_value,finding_key,first_seen_at) VALUES(?,?,?,?,?,?)",
        params![id.site_id, id.check_id, id.subject_type, id.subject_value, key, now],
    )?;
    Ok((con.last_insert_rowid(), true, key))
}

/// Insert (or find) an occurrence: `(occurrence_id, created, key)`.
pub fn insert_occurrence(
    con: &Connection,
    finding: &FindingIdentity<'_>,
    finding_id: i64,
    spec: &OccurrenceSpec,
    now: &str,
    refuse_new: bool,
) -> Result<(i64, bool, String), StoreError> {
    if !REPRESENTATIONS.contains(&spec.representation.as_str()) {
        return Err(ledger_error(format!(
            "unsupported occurrence representation '{}'",
            spec.representation
        )));
    }
    let key = occurrence_key(
        finding.project_uuid,
        finding.site_host,
        finding.check_key,
        &spec.subject_type,
        &spec.subject_value,
        &spec.representation,
        &spec.discriminator_type,
        &spec.discriminator_value,
    );
    let found: Option<i64> = con
        .query_row(
            "SELECT occurrence_id FROM occurrence WHERE occurrence_key=?",
            [&key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(found) = found {
        return Ok((found, false, key));
    }
    if refuse_new {
        return Err(conflicting());
    }
    con.execute(
        "INSERT INTO occurrence(finding_id,check_id,subject_type,subject_value,representation,discriminator_type,discriminator_value,occurrence_key,first_seen_at) VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            finding_id,
            finding.check_id,
            spec.subject_type,
            spec.subject_value,
            spec.representation,
            spec.discriminator_type,
            spec.discriminator_value,
            key,
            now
        ],
    )?;
    Ok((con.last_insert_rowid(), true, key))
}

/// Add one affected-URL member; true when it is new.
pub fn insert_member(
    con: &Connection,
    finding_id: i64,
    url: &str,
    role: &str,
    now: &str,
    refuse_new: bool,
) -> Result<bool, StoreError> {
    let changed = con.execute(
        "INSERT OR IGNORE INTO affected_url(finding_id,url,role,first_seen_at) VALUES(?,?,?,?)",
        params![finding_id, url, role, now],
    )?;
    let created = changed > 0;
    if created && refuse_new {
        return Err(conflicting());
    }
    Ok(created)
}

/// The retained projection of one issue under one source revision.
pub struct ProjectionRow {
    /// Audit issue id.
    pub issue_ordinal: String,
    /// Audit fingerprint, when the issue carries one.
    pub audit_fingerprint: Option<String>,
    /// Severity text.
    pub severity: String,
    /// HTTP status, when an integer.
    pub status_code: Option<i64>,
    /// Message text.
    pub message: String,
    /// Reported occurrence total.
    pub occurrences_count: i64,
    /// Enumerated occurrence total.
    pub enumerated_count: i64,
    /// Coverage classification.
    pub coverage_state: String,
    /// Canonical JSON of the issue.
    pub payload_json: String,
    /// SHA-256 of the payload.
    pub payload_sha256: String,
}

/// Insert one finding projection; false when the identical row exists.
pub fn insert_projection(
    con: &Connection,
    finding_id: i64,
    source_scan_id: i64,
    row: &ProjectionRow,
    refuse_new: bool,
) -> Result<bool, StoreError> {
    let existing: Option<String> = con
        .query_row(
            "SELECT payload_sha256 FROM finding_observation WHERE finding_id=? AND source_scan_id=? AND issue_ordinal=?",
            params![finding_id, source_scan_id, row.issue_ordinal],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != row.payload_sha256 {
            return Err(ledger_error(
                "conflicting finding projection under an identical source revision",
            ));
        }
        return Ok(false);
    }
    if refuse_new {
        return Err(conflicting());
    }
    con.execute(
        "INSERT INTO finding_observation(finding_id,source_scan_id,issue_ordinal,audit_fingerprint,severity,status_code,message,occurrences_count,enumerated_count,coverage_state,payload_json,payload_sha256) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            finding_id,
            source_scan_id,
            row.issue_ordinal,
            row.audit_fingerprint,
            row.severity,
            row.status_code,
            row.message,
            row.occurrences_count,
            row.enumerated_count,
            row.coverage_state,
            row.payload_json,
            row.payload_sha256
        ],
    )?;
    Ok(true)
}

/// A stored `source_group` row: check, value, count, member count, members digest.
type StoredSourceGroup = (String, Option<String>, Option<i64>, i64, String);

/// Store one source group and stream each member once, preserving its ordinal.
///
/// Returns `(created, complete)`.
pub fn store_source_group(
    con: &Connection,
    source_scan_id: i64,
    group: &Value,
    refuse_new: bool,
) -> Result<(bool, bool), StoreError> {
    let reference = group
        .get("group_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let check = match group.get("check") {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    };
    let value = group
        .get("value")
        .and_then(Value::as_str)
        .map(str::to_string);
    let count = group.get("count").and_then(as_int);
    let urls = group.get("urls").and_then(Value::as_array);
    let existing: Option<StoredSourceGroup> = con
        .query_row(
            "SELECT group_check,group_value,group_count,member_count,members_sha256 FROM source_group WHERE source_scan_id=? AND group_ref=?",
            params![source_scan_id, reference],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let clash = || ledger_error("conflicting group membership under an identical source revision");
    if let Some((c, v, n, _, _)) = &existing {
        if (c, v, n) != (&check, &value, &count) {
            return Err(clash());
        }
    }
    if existing.is_none() {
        if refuse_new {
            return Err(conflicting());
        }
        con.execute(
            "INSERT INTO source_group VALUES(?,?,?,?,?,0,'')",
            params![source_scan_id, reference, check, value, count],
        )?;
    }
    let mut digest = Sha256::new();
    let mut member_count: i64 = 0;
    let mut valid = urls.is_some();
    for (ordinal, member) in urls.into_iter().flatten().enumerate() {
        let raw = dump(member);
        let encoded = raw.as_bytes();
        if encoded.len() as i64 > MAX_PAYLOAD_BYTES {
            return Err(ledger_error(
                "group member exceeds the ledger payload bound",
            ));
        }
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
        valid = valid && member.as_str().is_some_and(|s| !s.trim().is_empty());
        if existing.is_none() {
            con.execute(
                "INSERT INTO source_group_member VALUES(?,?,?,?)",
                params![source_scan_id, reference, ordinal as i64, raw],
            )?;
        }
        member_count += 1;
    }
    let hex: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    match &existing {
        None => {
            con.execute(
                "UPDATE source_group SET member_count=?,members_sha256=? WHERE source_scan_id=? AND group_ref=?",
                params![member_count, hex, source_scan_id, reference],
            )?;
        }
        Some((_, _, _, stored_count, stored_sha)) => {
            if (*stored_count, stored_sha) != (member_count, &hex) {
                return Err(clash());
            }
        }
    }
    let unique: i64 = con.query_row(
        "SELECT COUNT(DISTINCT value_json) FROM source_group_member WHERE source_scan_id=? AND group_ref=?",
        params![source_scan_id, reference],
        |r| r.get(0),
    )?;
    Ok((
        existing.is_none(),
        valid && count == Some(member_count) && member_count == unique,
    ))
}

/// The group-membership row recorded for one finding under one source revision.
pub struct GroupRow {
    /// Group reference.
    pub group_ref: String,
    /// Check of the group.
    pub group_check: String,
    /// Group value.
    pub group_value: Option<String>,
    /// Group member count as reported.
    pub group_count: Option<i64>,
    /// Always `[]`: members live in `source_group_member`.
    pub group_urls_json: String,
}

/// Insert one finding-group membership; false when the identical row exists.
pub fn insert_group(
    con: &Connection,
    finding_id: i64,
    source_scan_id: i64,
    row: &GroupRow,
    refuse_new: bool,
) -> Result<bool, StoreError> {
    let existing: Option<(String, Option<String>, Option<i64>, String)> = con
        .query_row(
            "SELECT group_check,group_value,group_count,group_urls_json FROM finding_group WHERE finding_id=? AND source_scan_id=? AND group_ref=?",
            params![finding_id, source_scan_id, row.group_ref],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some(stored) = existing {
        let presented = (
            row.group_check.clone(),
            row.group_value.clone(),
            row.group_count,
            row.group_urls_json.clone(),
        );
        if stored != presented {
            return Err(ledger_error(
                "conflicting group membership under an identical source revision",
            ));
        }
        return Ok(false);
    }
    if refuse_new {
        return Err(conflicting());
    }
    con.execute(
        "INSERT INTO finding_group(finding_id,source_scan_id,group_ref,group_check,group_value,group_count,group_urls_json) VALUES(?,?,?,?,?,?,?)",
        params![
            finding_id,
            source_scan_id,
            row.group_ref,
            row.group_check,
            row.group_value,
            row.group_count,
            row.group_urls_json
        ],
    )?;
    Ok(true)
}

/// One immutable sighting of an occurrence.
pub struct ObservationRow {
    /// `target` or `source`.
    pub role: String,
    /// Audit issue id.
    pub issue_ordinal: String,
    /// Evidence state.
    pub evidence_state: String,
    /// Evidence reason.
    pub evidence_reason: String,
    /// Canonical JSON of the evidence.
    pub evidence_json: String,
    /// SHA-256 of the occurrence payload.
    pub payload_sha256: String,
    /// Results-policy fingerprint.
    pub config_fingerprint: Value,
    /// Producer version.
    pub producer_version: Value,
    /// Producer revision.
    pub producer_revision: Value,
    /// Observation time, when known.
    pub observed_at: Option<String>,
    /// `known` or `unknown`.
    pub observed_at_state: String,
    /// Ingest time.
    pub ingested_at: String,
}

fn sql_value(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => rusqlite::types::Value::Integer(i),
            None => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        other => rusqlite::types::Value::Text(other.to_string()),
    }
}

/// Insert one observation; false when the identical row exists.
pub fn insert_observation(
    con: &Connection,
    occurrence_id: i64,
    source_scan_id: i64,
    row: &ObservationRow,
    refuse_new: bool,
) -> Result<bool, StoreError> {
    let existing: Option<String> = con
        .query_row(
            "SELECT payload_sha256 FROM observation WHERE occurrence_id=? AND source_scan_id=? AND issue_ordinal=?",
            params![occurrence_id, source_scan_id, row.issue_ordinal],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != row.payload_sha256 {
            return Err(ledger_error(
                "conflicting observation bytes under an identical source revision",
            ));
        }
        return Ok(false);
    }
    if refuse_new {
        return Err(conflicting());
    }
    let revision: i64 = con.query_row(
        "SELECT COALESCE(MAX(observation_revision),0)+1 FROM observation WHERE occurrence_id=?",
        [occurrence_id],
        |r| r.get(0),
    )?;
    con.execute(
        "INSERT INTO observation(occurrence_id,source_scan_id,observation_revision,role,issue_ordinal,evidence_state,evidence_reason,evidence_json,payload_sha256,config_fingerprint,producer_version,producer_revision,observed_at,observed_at_state,ingested_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            occurrence_id,
            source_scan_id,
            revision,
            row.role,
            row.issue_ordinal,
            row.evidence_state,
            row.evidence_reason,
            row.evidence_json,
            row.payload_sha256,
            sql_value(&row.config_fingerprint),
            sql_value(&row.producer_version),
            sql_value(&row.producer_revision),
            row.observed_at,
            row.observed_at_state,
            row.ingested_at
        ],
    )?;
    Ok(true)
}

/// Outcome of binding a source revision.
pub struct Binding {
    /// Row id of the bound source scan.
    pub source_scan_id: i64,
    /// True when this call created the binding.
    pub created: bool,
    /// Container bytes differ from the first seen digest.
    pub bytes_differ: bool,
    /// A `missing` artifact was proven present again.
    pub restored: bool,
}

/// Bind one validated source revision; refuse an inconsistent reused identity.
///
/// `bytes_differ` marks a same-revision binding whose container bytes differ from the first seen
/// digest (legitimate after pin/copy), where the ingest must reproduce the recorded content
/// exactly or fail rather than mint alternate truth. `restored` records a bound artifact marked
/// `missing` that a successful re-ingest proved present again.
#[allow(clippy::too_many_arguments)]
pub fn bind_source(
    con: &Connection,
    site_id: i64,
    scan: &Record,
    audit_row: &Record,
    canonical_audit_sha256: Option<&str>,
    file_sha256: &str,
    group_memberships_state: &str,
    now: &str,
) -> Result<Binding, StoreError> {
    let get = |m: &Record, k: &str| m.get(k).cloned().unwrap_or(Value::Null);
    let flag = |k: &str| json!(i64::from(truthy(&get(scan, k))));
    let scan_uuid = get(scan, "scan_uuid");
    let revision = get(scan, "evidence_revision");
    let rows = query_maps(
        con,
        "SELECT * FROM source_scan WHERE scan_uuid=? AND evidence_revision=?",
        [sql_value(&scan_uuid), sql_value(&revision)],
    )?;
    let mut fields = Record::new();
    fields.insert("site_id".into(), json!(site_id));
    fields.insert("scan_uuid".into(), scan_uuid.clone());
    fields.insert("format_version".into(), get(scan, "format_version"));
    fields.insert("source_kind".into(), get(scan, "source_kind"));
    fields.insert("lifecycle".into(), get(scan, "lifecycle"));
    fields.insert("evidence_revision".into(), revision.clone());
    fields.insert("audit_sha256".into(), get(audit_row, "sha256"));
    fields.insert(
        "canonical_audit_sha256".into(),
        canonical_audit_sha256.map_or(Value::Null, |s| json!(s)),
    );
    fields.insert(
        "audit_schema_version".into(),
        get(audit_row, "schema_version"),
    );
    fields.insert("audit_created_at".into(), get(audit_row, "created_at"));
    fields.insert("config_fingerprint".into(), get(scan, "config_fingerprint"));
    fields.insert("writer_version".into(), get(scan, "writer_version"));
    fields.insert("writer_revision".into(), get(scan, "writer_revision"));
    fields.insert(
        "analyzer_version".into(),
        get(audit_row, "analyzer_version"),
    );
    fields.insert(
        "analyzer_revision".into(),
        get(audit_row, "analyzer_revision"),
    );
    fields.insert("scan_sha256".into(), json!(file_sha256));
    fields.insert("crawl_partial".into(), flag("crawl_partial"));
    fields.insert("corpus_partial".into(), flag("corpus_partial"));
    fields.insert("artifact_state".into(), json!("present"));
    fields.insert("missing_reason".into(), json!(""));
    fields.insert(
        "group_memberships_state".into(),
        json!(group_memberships_state),
    );
    fields.insert("ingested_at".into(), json!(now));
    let Some(stored) = rows.into_iter().next() else {
        let names = [
            "site_id",
            "scan_uuid",
            "format_version",
            "source_kind",
            "lifecycle",
            "evidence_revision",
            "audit_sha256",
            "canonical_audit_sha256",
            "audit_schema_version",
            "audit_created_at",
            "config_fingerprint",
            "writer_version",
            "writer_revision",
            "analyzer_version",
            "analyzer_revision",
            "scan_sha256",
            "crawl_partial",
            "corpus_partial",
            "artifact_state",
            "missing_reason",
            "group_memberships_state",
            "ingested_at",
        ];
        let values: Vec<rusqlite::types::Value> =
            names.iter().map(|n| sql_value(&fields[*n])).collect();
        con.execute(
            "INSERT INTO source_scan(site_id,scan_uuid,format_version,source_kind,lifecycle,evidence_revision,audit_sha256,canonical_audit_sha256,audit_schema_version,audit_created_at,config_fingerprint,writer_version,writer_revision,analyzer_version,analyzer_revision,scan_sha256,crawl_partial,corpus_partial,artifact_state,missing_reason,group_memberships_state,ingested_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params_from_iter(values.iter()),
        )?;
        return Ok(Binding {
            source_scan_id: con.last_insert_rowid(),
            created: true,
            bytes_differ: false,
            restored: false,
        });
    };
    for name in [
        "site_id",
        "format_version",
        "source_kind",
        "lifecycle",
        "audit_sha256",
        "audit_schema_version",
        "config_fingerprint",
        "writer_version",
        "writer_revision",
        "analyzer_version",
        "analyzer_revision",
        "crawl_partial",
        "corpus_partial",
    ] {
        if stored[name] != fields[name] {
            return Err(ledger_error(format!(
                "source scan {} revision {} was already bound with different {name}; refusing alternate truth",
                py_str(&scan_uuid),
                py_str(&revision)
            )));
        }
    }
    let stored_id = as_int(&stored["source_scan_id"]).unwrap_or(0);
    let stored_canonical = stored["canonical_audit_sha256"].clone();
    if stored_canonical.is_null() && canonical_audit_sha256.is_some() {
        con.execute(
            "UPDATE source_scan SET canonical_audit_sha256=? WHERE source_scan_id=?",
            params![canonical_audit_sha256, stored_id],
        )?;
    } else if stored_canonical != fields["canonical_audit_sha256"] {
        return Err(ledger_error(format!(
            "source scan {} revision {} was already bound with a different canonical audit digest; refusing alternate truth",
            py_str(&scan_uuid),
            py_str(&revision)
        )));
    }
    let restored = stored["artifact_state"].as_str() == Some("missing");
    if restored {
        con.execute(
            "UPDATE source_scan SET artifact_state='present',missing_reason='' WHERE source_scan_id=?",
            [stored_id],
        )?;
    }
    Ok(Binding {
        source_scan_id: stored_id,
        created: false,
        bytes_differ: stored["scan_sha256"].as_str() != Some(file_sha256),
        restored,
    })
}
