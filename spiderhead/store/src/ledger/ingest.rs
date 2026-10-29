//! Writing the remediation ledger: registration, ingest and bound-source bookkeeping.
//!
//! Everything that appends to a ledger artifact lives here (and in [`super::lifecycle`] for the
//! lifecycle decisions): `create_ledger` and `register_site` bootstrap the project/site identity,
//! [`ingest_scan`] binds one validated source revision and records findings, occurrences,
//! affected-URL membership, group memberships and immutable observations. Reading and reporting
//! live in `queries`; the schema, migrations and connection handling live in `schema`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use indexmap::IndexMap;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::schema::{
    apply_v2_schema, apply_v3_schema, apply_v4_schema, apply_v5_schema, canonical_url, is_revision,
    ledger_error, open_ledger, runtime, schema, FORMAT_VERSION, MAX_PAYLOAD_BYTES, USER_VERSION,
};
use super::urlparts::{urlsplit, urlunsplit};
use super::LedgerRef;
use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::dump;
use crate::scan_lease::utc;

pub use super::lifecycle::{
    finding_state, lifecycle_text, occurrence_matches_baseline, outcome_observation,
    record_verification, transition_occurrence,
};
pub use super::rows::*;
pub use super::specs::*;

/// A JSON object.
pub type Record = Map<String, Value>;

/// True only for integers; booleans do not count.
pub(crate) fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Truthiness of a JSON value.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Text of a JSON scalar, for values that may not be strings.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Hash one retained JSON value without depending on its file formatting.
pub fn json_sha256(value: &Value) -> String {
    sha256_hex(spiderhead_core::common::canonical_json(value).as_bytes())
}

/// The value when it is an ISO-8601 timestamp with a zero UTC offset (the only form recorded).
pub fn validated_time(value: &Value) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let text = value.as_str().filter(|s| !s.is_empty())?;
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2})(?::?(\d{2})(?::?(\d{2})(?:[.,]\d{1,6})?)?)?)?(?:\+|-)00(?::?00(?::?00)?)?$",
        )
        .expect("static pattern")
    });
    let replaced = text.replace('Z', "+00:00");
    let caps = re.captures(&replaced)?;
    let num = |i: usize| caps.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let (year, month, day) = (num(1)?, num(2)?, num(3)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + u32::from(leap),
        _ => return None,
    };
    if year == 0 || day == 0 || day > days {
        return None;
    }
    if num(4).is_some_and(|h| h > 23)
        || num(5).is_some_and(|m| m > 59)
        || num(6).is_some_and(|s| s > 59)
    {
        return None;
    }
    Some(text.to_string())
}

/// The registered site with this netloc, primary first.
pub fn site_row(con: &Connection, netloc: &str) -> Result<Option<Record>, StoreError> {
    let rows = query_maps(
        con,
        "SELECT * FROM site WHERE netloc=? ORDER BY (role='primary') DESC, site_id LIMIT 1",
        [netloc],
    )?;
    Ok(rows.into_iter().next())
}

/// A bounded public HTTP(S) site identity.
pub fn normalize_target(value: &str) -> Result<String, StoreError> {
    let bad = |m: &str| StoreError::Invalid(m.to_string());
    if value.chars().count() > 2048 {
        return Err(bad("target must be a bounded absolute HTTP(S) URL"));
    }
    let raw = urlsplit(value);
    if raw.username().is_some_and(|u| !u.is_empty())
        || raw.password().is_some_and(|p| !p.is_empty())
        || !raw.query.is_empty()
        || !raw.fragment.is_empty()
    {
        return Err(bad(
            "target site identity must not include credentials, a query, or a fragment",
        ));
    }
    let normalized = spiderhead_core::net::normalize_url(value);
    if normalized.is_empty() {
        return Err(bad(
            "target must be a public HTTP(S) site identity without credentials",
        ));
    }
    let parts = urlsplit(&normalized);
    let Some(hostname) = parts.hostname() else {
        return Err(bad(
            "target site identity must not include a query or fragment",
        ));
    };
    if !parts.query.is_empty() || !parts.fragment.is_empty() {
        return Err(bad(
            "target site identity must not include a query or fragment",
        ));
    }
    let host = spiderhead_core::net::normalize_domain(&hostname);
    if host.is_empty() {
        return Err(bad("target must have a valid public hostname"));
    }
    let port = match parts.port() {
        Err(_) => return Err(bad("target must have a valid HTTP(S) port")),
        Ok(Some(p)) if p != 0 => format!(":{p}"),
        Ok(_) => String::new(),
    };
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    Ok(urlunsplit(
        &parts.scheme.to_lowercase(),
        &format!("{host}{port}"),
        path,
        "",
        "",
    ))
}

fn temp_name(parent: &Path) -> PathBuf {
    parent.join(format!(".ledger-{}.sqlite", uuid::Uuid::new_v4().simple()))
}

/// Create one new empty ledger bound to a validated project; never overwrites.
///
/// `writer_version` is the producing toolkit's version string; `project_uuid` and `target` come
/// from the validated project manifest (the caller opens the project).
pub fn create_ledger(
    path: &Path,
    project_uuid: &str,
    target: &str,
    writer_version: &str,
    producer_build: &str,
) -> Result<PathBuf, StoreError> {
    runtime()?;
    if !is_revision(producer_build) {
        return Err(ledger_error(
            "producer_build must identify the ledger-writing build with a full lowercase Git SHA",
        ));
    }
    let out = std::path::absolute(path)?;
    let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let already = || {
        ledger_error(format!(
            "ledger already exists: {}; ledgers never overwrite",
            out.display()
        ))
    };
    if exists(&out) {
        return Err(already());
    }
    let parent = out.parent().map(Path::to_path_buf).unwrap_or_default();
    if !parent.is_dir() {
        return Err(ledger_error(format!(
            "ledger parent directory does not exist: {}",
            parent.display()
        )));
    }
    let temporary = temp_name(&parent);
    let result = (|| -> Result<(), StoreError> {
        let con = Connection::open(&temporary)?;
        con.execute_batch(&schema())?;
        apply_v2_schema(&con)?;
        apply_v3_schema(&con)?;
        apply_v4_schema(&con)?;
        apply_v5_schema(&con)?;
        con.execute_batch(&format!("PRAGMA user_version={USER_VERSION}"))?;
        con.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL",
        )?;
        con.execute_batch("BEGIN")?;
        let now = utc()?;
        con.execute(
            "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
            params![
                uuid::Uuid::new_v4().to_string(),
                FORMAT_VERSION,
                now,
                writer_version,
                producer_build,
                0
            ],
        )?;
        register_site(&con, project_uuid, target, "primary", Some(&now))?;
        con.execute_batch("COMMIT")?;
        drop(con);
        drop(open_ledger(&temporary, false)?);
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        std::fs::hard_link(&temporary, &out).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                already()
            } else {
                e.into()
            }
        })?;
        std::fs::File::open(&parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    match result {
        Ok(()) => Ok(out),
        Err(StoreError::LedgerError(e)) => Err(StoreError::LedgerError(e)),
        Err(StoreError::Io(e)) => Err(ledger_error(format!("cannot create ledger: {e}"))),
        Err(StoreError::Sqlite(e)) => Err(ledger_error(format!("cannot create ledger: {e}"))),
        Err(StoreError::Invalid(m)) => Err(ledger_error(format!("cannot create ledger: {m}"))),
        Err(other) => Err(other),
    }
}

/// Register one site identity on an open write connection.
///
/// `target` is normalized with the same site-target rules the project workspace applies; scans
/// bind by exact `netloc` match. A repeat call with an identical (project_uuid, netloc) pair
/// returns the existing row.
pub fn register_site(
    con: &Connection,
    project_uuid: &str,
    target: &str,
    role: &str,
    created_at: Option<&str>,
) -> Result<i64, StoreError> {
    let normalized_uuid = uuid::Uuid::parse_str(project_uuid)
        .map_err(|_| ledger_error("site registration requires a project UUID"))?
        .hyphenated()
        .to_string();
    if role != "primary" && role != "secondary" {
        return Err(ledger_error("site role must be 'primary' or 'secondary'"));
    }
    let normalized = normalize_target(target)?;
    let parts = urlsplit(&normalized);
    let host = spiderhead_core::net::normalize_domain(&parts.hostname().unwrap_or_default());
    let netloc = parts.netloc.to_lowercase();
    if host.is_empty() || netloc.is_empty() {
        return Err(ledger_error("site registration requires a public hostname"));
    }
    let row: Option<(i64, String, String)> = con
        .query_row(
            "SELECT site_id,target,host FROM site WHERE project_uuid=? AND netloc=?",
            params![normalized_uuid, netloc],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((id, stored_target, stored_host)) = row {
        if stored_target != normalized || stored_host != host {
            return Err(ledger_error(
                "site identity conflicts with a registered row",
            ));
        }
        return Ok(id);
    }
    let created = created_at.map(str::to_string).map_or_else(utc, Ok)?;
    con.execute(
        "INSERT INTO site(project_uuid,target,host,netloc,role,created_at) VALUES(?,?,?,?,?,?)",
        params![normalized_uuid, normalized, host, netloc, role, created],
    )?;
    Ok(con.last_insert_rowid())
}

/// A source scan and its audit, projected and ready to be written into a ledger.
pub struct PreparedSource<'a> {
    /// The scan header row (with the effective `config_fingerprint`).
    pub scan: Record,
    /// Audit identity: `sha256`, `schema_version`, `created_at`, `analyzer_version`, `analyzer_revision`.
    pub audit_row: Record,
    /// The (projected) audit document; only its `run` block is read here.
    pub document: Value,
    /// Digest of the canonical legacy document, when there is one.
    pub canonical_audit_sha256: Option<String>,
    /// Projected issues in audit order (a stream: a million-row audit is never held at once).
    pub issues: Box<dyn Iterator<Item = Result<Value, StoreError>> + 'a>,
    /// The raw issue per ordinal for legacy documents; `None` for audit.v2.
    pub raw_issues: Option<Vec<Value>>,
    /// Group objects, when the source retains them.
    pub groups: Option<Vec<Value>>,
    /// URL to recorded representation.
    pub representations: BTreeMap<String, String>,
    /// Page URL to owned observation ids.
    pub by_url: BTreeMap<String, BTreeSet<String>>,
    /// Document id to page URL.
    pub doc_url: BTreeMap<i64, String>,
    /// Initial group-membership scope state.
    pub group_memberships_state: String,
}

/// Fingerprint of the effective results policy, including the validated adjunct.
fn effective_config_fingerprint(
    scan_con: &Connection,
    scan: &Record,
    has_extension: bool,
) -> Result<String, StoreError> {
    let fingerprint = |value: &Value| {
        spiderhead_core::crawl_settings::fingerprint(value, &spiderhead_core::host::SystemHost)
            .map_err(|e| StoreError::Invalid(e.to_string()))
    };
    let effective = crate::scan_schema::read_effective_config(scan_con)?;
    if has_extension {
        return fingerprint(&Value::Object(effective));
    }
    let text = scan
        .get("config_json")
        .and_then(Value::as_str)
        .ok_or_else(|| ledger_error("source scan has no config header"))?;
    let mut historical = crate::scan_artifact::loads(text, "config")?;
    let thresholds = effective
        .get("analysis")
        .and_then(|a| a.get("asset_thresholds"))
        .cloned()
        .ok_or_else(|| ledger_error("effective config has no asset thresholds"))?;
    let root = historical
        .as_object_mut()
        .ok_or_else(|| ledger_error("source scan config is not an object"))?;
    let analysis = root
        .entry("analysis")
        .or_insert_with(|| Value::Object(Map::new()));
    match analysis.as_object_mut() {
        Some(map) => {
            map.insert("asset_thresholds".into(), thresholds);
        }
        None => return Err(ledger_error("source scan analysis config is not an object")),
    }
    fingerprint(&historical)
}

/// Project the saved audit (internal document or audit.v2 companion) through the evidence
/// contract and index the scan's own observations (the body of the source
/// block). The companion's issues are projected eagerly here.
fn projected_source<'a>(
    path: &Path,
    scan_con: &Connection,
    scan: &mut Record,
) -> Result<PreparedSource<'a>, StoreError> {
    let scan_uuid = scan
        .get("scan_uuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let audit = query_maps(scan_con, "SELECT * FROM audit WHERE singleton=1", [])?
        .into_iter()
        .next();
    if let Some(audit_row) = audit {
        let text = audit_row
            .get("document_json")
            .and_then(Value::as_str)
            .unwrap_or("");
        let document = crate::scan_artifact::loads(text, "audit")?;
        let canonical_audit_sha256 = Some(json_sha256(&document));
        let object = document
            .as_object()
            .ok_or_else(|| ledger_error("saved audit is not an object"))?;
        let projected = crate::evidence_contract::attach_contract(
            object,
            scan_uuid.as_deref(),
            Some(scan_con),
        )?;
        let issues: Vec<Value> = projected
            .get("issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let raw_issues = document
            .get("issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let groups = document
            .get("groups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let representations = super::specs::representation_map(scan_con, &document);
        let (by_url, doc_url) = super::specs::observation_index(scan_con);
        return Ok(PreparedSource {
            scan: scan.clone(),
            audit_row,
            document,
            canonical_audit_sha256,
            issues: Box::new(issues.into_iter().map(Ok)),
            raw_issues: Some(raw_issues),
            groups: Some(groups),
            representations,
            by_url,
            doc_url,
            group_memberships_state: "complete".into(),
        });
    }
    let reader = crate::audit_v2::AuditV2Reader::new(path, true, false, None)?;
    let header = reader
        .header
        .as_object()
        .cloned()
        .ok_or_else(|| ledger_error("audit.v2 header is not an object"))?;
    if header.get("schema_version").and_then(Value::as_str) != Some("2.0") {
        return Err(ledger_error(
            "audit.v2 header has an unsupported schema version",
        ));
    }
    if !reader.collections.contains_key("/issues") {
        return Err(ledger_error(
            "audit.v2 source has no retained issue collection",
        ));
    }
    let raw = reader.iter_collection("/issues")?;
    let input = spiderhead_formats::models::Rows::new(|| Box::new(raw.clone().into_iter()));
    let (projected, stream) = crate::evidence_contract::attach_contract_parts(
        &header,
        &input,
        scan_uuid.as_deref(),
        None,
    )?;
    let issues: Vec<Value> = stream.iter().collect();
    let mut audit_row = Record::new();
    audit_row.insert("sha256".into(), json!(reader.sha256));
    audit_row.insert(
        "schema_version".into(),
        projected
            .get("schema_version")
            .cloned()
            .unwrap_or(Value::Null),
    );
    audit_row.insert(
        "created_at".into(),
        projected
            .get("run")
            .and_then(|r| r.get("generated_at"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    for key in ["analyzer_version", "analyzer_revision"] {
        audit_row.insert(
            key.into(),
            reader.binding.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    let (groups, state) = if reader.collections.contains_key("/groups") {
        (Some(reader.iter_collection("/groups")?), "complete")
    } else {
        (None, "unavailable")
    };
    // A group's member sequence is lazy in the reader; the ledger writer reads it whole.
    let groups = match groups {
        None => None,
        Some(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for mut row in rows {
                if let Some(members) = reader.group_members(&row) {
                    let urls = members.to_vec()?;
                    if let Some(map) = row.as_object_mut() {
                        map.insert("urls".into(), Value::Array(urls));
                    }
                }
                out.push(row);
            }
            Some(out)
        }
    };
    Ok(PreparedSource {
        scan: scan.clone(),
        audit_row,
        document: Value::Object(projected),
        canonical_audit_sha256: None,
        issues: Box::new(issues.into_iter().map(Ok)),
        raw_issues: None,
        groups,
        representations: BTreeMap::new(),
        by_url: BTreeMap::new(),
        doc_url: BTreeMap::new(),
        group_memberships_state: state.into(),
    })
}

/// Open the saved scan read-only and project its audit into a [`PreparedSource`].
pub fn prepare_source<'a>(path: &Path) -> Result<PreparedSource<'a>, StoreError> {
    let scan_con = crate::scan_artifact::open_scan(path, false, None)?;
    let scan = query_maps(&scan_con, "SELECT * FROM scan WHERE singleton=1", [])?
        .into_iter()
        .next()
        .ok_or_else(|| ledger_error("source scan has no header to ingest"))?;
    let mut scan = scan;
    let marker = |kind: &str| -> Result<bool, StoreError> {
        Ok(scan_con
            .query_row(
                "SELECT 1 FROM context_items WHERE kind=? AND item_key='run'",
                [kind],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    let has_extension = marker("native_config_extension")?;
    let has_thresholds = marker("analysis_asset_thresholds")?;
    let kind = scan
        .get("source_kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    if (kind == "native" || kind == "reanalysis") && (has_extension || has_thresholds) {
        // Bind the measured results policy, including the validated adjunct, while the source
        // artifact keeps its frozen header and bytes.
        let fingerprint = effective_config_fingerprint(&scan_con, &scan, has_extension)?;
        scan.insert("config_fingerprint".into(), json!(fingerprint));
    }
    projected_source(path, &scan_con, &mut scan)
}

/// Ingest one validated saved audit as baseline/history without rerunning it.
///
/// The source artifact is opened strictly read-only: ingest never writes to the scan, never
/// upgrades `scan.v1` to `scan.v2`, and leaves its `user_version` and bytes untouched.
/// Re-ingesting the same source revision is idempotent; a new revision appends observation
/// history. `ledger` may be a path (opened write-mode for this call) or an already open write
/// connection, which receives the whole ingest in one transaction.
pub fn ingest_scan(ledger: LedgerRef<'_>, scan_path: &Path) -> Result<Record, StoreError> {
    let digest = spiderhead_core::filesystem::file_sha256(scan_path)?;
    let prepared = prepare_source(scan_path)?;
    let con = ledger.open(true)?;
    ingest_prepared(&con, prepared, &digest, &utc()?)
}

/// Write a prepared source into an open write connection, in one transaction.
///
/// `now` stamps every row created by this call.
pub fn ingest_prepared(
    con: &Connection,
    mut prepared: PreparedSource<'_>,
    scan_sha256: &str,
    now: &str,
) -> Result<Record, StoreError> {
    let scan_uuid = prepared
        .scan
        .get("scan_uuid")
        .cloned()
        .unwrap_or(Value::Null);
    let source_kind = prepared
        .scan
        .get("source_kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let run = prepared
        .document
        .get("run")
        .filter(|r| r.is_object())
        .cloned()
        .unwrap_or(Value::Null);
    let document_uuid = run
        .get("scan_uuid")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty());
    // A native/reanalysis audit's own scan_uuid must agree with its container; a legacy import
    // carries the *original* run's uuid as provenance while the binding uuid was minted at
    // import time, so it is compared as data instead.
    if source_kind != "legacy_import"
        && document_uuid.is_some_and(|d| Some(d) != scan_uuid.as_str())
    {
        return Err(ledger_error(
            "saved audit scan UUID disagrees with its scan header",
        ));
    }
    let start = [
        prepared.scan.get("start_url"),
        run.get("start_url"),
        run.get("source"),
    ]
    .into_iter()
    .flatten()
    .find(|v| truthy(v));
    let netloc = start
        .and_then(Value::as_str)
        .map(|s| urlsplit(s).netloc.to_lowercase())
        .unwrap_or_default();
    if netloc.is_empty() {
        return Err(ledger_error(
            "source scan does not record the site it belongs to",
        ));
    }
    let observed_at = run.get("generated_at").and_then(validated_time);
    let site = site_row(con, &netloc)?.ok_or_else(|| {
        // Registered netlocs never carry userinfo, so a credential-bearing start_url always
        // lands here; report host[:port] without leaking the credentials into the error text.
        let shown = start.and_then(Value::as_str).map(|s| {
            let parts = urlsplit(s);
            let host = parts.hostname().unwrap_or_default();
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host
            };
            match parts.port() {
                Ok(Some(port)) => format!("{host}:{port}"),
                _ => host,
            }
        });
        let shown = shown
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| netloc.rsplit('@').next().unwrap_or("").to_string());
        ledger_error(format!(
            "scan site '{shown}' does not match any registered ledger site identity"
        ))
    })?;
    con.execute_batch("BEGIN IMMEDIATE")?;
    let mut group_ids_created = false;
    let outcome = write_source(
        con,
        &mut prepared,
        &Context {
            site: &site,
            netloc: &netloc,
            scan_sha256,
            now,
            observed_at,
        },
        &mut group_ids_created,
    );
    let finish = match &outcome {
        Ok(_) => con.execute_batch("COMMIT"),
        Err(_) => con.execute_batch("ROLLBACK"),
    };
    if group_ids_created {
        let _ = con.execute_batch("DROP TABLE IF EXISTS temp.ingest_group_ids");
    }
    let result = outcome?;
    finish?;
    Ok(result)
}

struct Context<'a> {
    site: &'a Record,
    netloc: &'a str,
    scan_sha256: &'a str,
    now: &'a str,
    observed_at: Option<String>,
}

fn text_of(value: &Value) -> String {
    if truthy(value) {
        py_str(value)
    } else {
        String::new()
    }
}

fn write_source(
    con: &Connection,
    prepared: &mut PreparedSource<'_>,
    ctx: &Context<'_>,
    group_ids_created: &mut bool,
) -> Result<Record, StoreError> {
    let now = ctx.now;
    let site_id = as_int(&ctx.site["site_id"]).unwrap_or(0);
    let site_uuid = ctx.site["project_uuid"].as_str().unwrap_or("").to_string();
    let site_host = ctx.site["host"].as_str().unwrap_or("").to_string();
    let mut counts: IndexMap<&str, i64> = [
        "findings",
        "occurrences",
        "affected_urls",
        "finding_observations",
        "group_memberships",
        "observations",
    ]
    .into_iter()
    .map(|k| (k, 0))
    .collect();
    let mut group_memberships_state = prepared.group_memberships_state.clone();
    let binding = bind_source(
        con,
        site_id,
        &prepared.scan,
        &prepared.audit_row,
        prepared.canonical_audit_sha256.as_deref(),
        ctx.scan_sha256,
        &group_memberships_state,
        now,
    )?;
    let source_scan_id = binding.source_scan_id;
    con.execute_batch("CREATE TEMP TABLE ingest_group_ids (group_ref TEXT PRIMARY KEY)")?;
    *group_ids_created = true;
    let has_groups = prepared.groups.is_some();
    for group in prepared.groups.iter().flatten() {
        let Some(group_id) = group
            .get("group_id")
            .and_then(Value::as_str)
            .filter(|g| !g.is_empty())
        else {
            group_memberships_state = "partial".into();
            continue;
        };
        if let Err(e) = con.execute("INSERT INTO ingest_group_ids VALUES(?)", [group_id]) {
            return Err(match e.sqlite_error_code() {
                Some(rusqlite::ErrorCode::ConstraintViolation) => {
                    ledger_error("saved audit contains duplicate group identities")
                }
                _ => e.into(),
            });
        }
        let (_created, complete) =
            store_source_group(con, source_scan_id, group, !binding.created)?;
        if !complete {
            group_memberships_state = "partial".into();
        }
    }
    // Once a source revision is bound, a replay must reproduce its recorded rows exactly: a new
    // row under a bound revision means the same claimed identity now presents different
    // content, which is refused, not merged.
    let refuse_new = !binding.created;
    for (ordinal, next) in prepared.issues.by_ref().enumerate() {
        let projected = next?;
        if !projected.is_object() {
            return Err(ledger_error("saved audit issue is not an object"));
        }
        let issue = prepared
            .raw_issues
            .as_ref()
            .and_then(|raw| raw.get(ordinal))
            .unwrap_or(&projected)
            .clone();
        let check_value = projected.get("check").cloned().unwrap_or(Value::Null);
        let check_row = check_id(con, &check_value, refuse_new)?;
        let check_key = check_value.as_str().unwrap_or("").to_string();
        let target = projected
            .get("target_url")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty());
        let (subject_type, subject_value) = match target {
            Some(t) => ("url", canonical_url(t)?),
            None => ("scope", SCOPE_SUBJECT.to_string()),
        };
        let identity = FindingIdentity {
            site_id,
            check_id: check_row,
            check_key: &check_key,
            project_uuid: &site_uuid,
            site_host: &site_host,
            subject_type,
            subject_value: &subject_value,
        };
        let (finding_id, created, _) = insert_finding(con, &identity, now, refuse_new)?;
        *counts.get_mut("findings").unwrap_or(&mut 0) += i64::from(created);
        let members = affected_urls(&projected)?;
        for (url, role) in &members {
            let added = insert_member(con, finding_id, url, role, now, refuse_new)?;
            *counts.get_mut("affected_urls").unwrap_or(&mut 0) += i64::from(added);
        }
        let contract = projected
            .get("evidence")
            .filter(|e| e.is_object())
            .map(|e| e.get("contract").cloned().unwrap_or_else(|| json!({})))
            .unwrap_or_else(|| json!({}));
        let specs = occurrence_specs(
            &projected,
            subject_type,
            &subject_value,
            &members,
            &prepared.representations,
        );
        let locations: Vec<Value> = projected
            .get("locations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (coverage, enumerated) = coverage_state(&projected, &locations);
        let payload = dump(&issue);
        if payload.len() as i64 > MAX_PAYLOAD_BYTES {
            return Err(ledger_error(
                "saved issue projection exceeds the ledger payload bound",
            ));
        }
        let issue_ordinal = text_of(projected.get("id").unwrap_or(&Value::Null));
        let projection_added = insert_projection(
            con,
            finding_id,
            source_scan_id,
            &ProjectionRow {
                issue_ordinal: issue_ordinal.clone(),
                audit_fingerprint: projected
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                severity: text_of(projected.get("severity").unwrap_or(&Value::Null)),
                status_code: projected.get("status_code").and_then(as_int),
                message: text_of(projected.get("message").unwrap_or(&Value::Null)),
                occurrences_count: projected
                    .get("occurrences_count")
                    .and_then(as_int)
                    .unwrap_or(0),
                enumerated_count: enumerated,
                coverage_state: coverage.clone(),
                payload_json: payload.clone(),
                payload_sha256: sha256_hex(payload.as_bytes()),
            },
            refuse_new,
        )?;
        *counts.get_mut("finding_observations").unwrap_or(&mut 0) += i64::from(projection_added);
        let group_ref = projected
            .get("group_id")
            .and_then(Value::as_str)
            .filter(|g| !g.is_empty());
        if let (Some(group_ref), true) = (group_ref, has_groups) {
            let stored = query_maps(
                con,
                "SELECT * FROM source_group WHERE source_scan_id=? AND group_ref=?",
                params![source_scan_id, group_ref],
            )?
            .into_iter()
            .next();
            let group = match stored {
                Some(row) => row,
                None => {
                    group_memberships_state = "partial".into();
                    Record::new()
                }
            };
            let added = insert_group(
                con,
                finding_id,
                source_scan_id,
                &GroupRow {
                    group_ref: group_ref.to_string(),
                    group_check: match group.get("group_check") {
                        Some(v) if truthy(v) => py_str(v),
                        _ => check_key.clone(),
                    },
                    group_value: group
                        .get("group_value")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    group_count: group.get("group_count").and_then(as_int),
                    group_urls_json: "[]".into(),
                },
                refuse_new,
            )?;
            *counts.get_mut("group_memberships").unwrap_or(&mut 0) += i64::from(added);
        } else if group_ref.is_some() {
            // The audit.v2 stream retained the issue's group reference but this ledger reader
            // has not materialized a group lookup table. Keep that loss explicit in the ingest
            // result rather than inventing an empty group membership.
            if group_memberships_state != "partial" {
                group_memberships_state = "unavailable".into();
            }
        }
        for spec in &specs {
            let (occurrence_id, created, _) =
                insert_occurrence(con, &identity, finding_id, spec, now, refuse_new)?;
            *counts.get_mut("occurrences").unwrap_or(&mut 0) += i64::from(created);
            let evidence = occurrence_evidence(
                &contract,
                &spec.role,
                (spec.subject_type == "url").then_some(spec.subject_value.as_str()),
                &prepared.by_url,
                &prepared.doc_url,
            );
            let evidence_json = dump(&evidence);
            let occurrence_payload = dump(&json!({
                "occurrence": spec.to_value(),
                "issue_ordinal": issue_ordinal,
                "severity": projected.get("severity").cloned().unwrap_or(Value::Null),
                "status_code": projected.get("status_code").cloned().unwrap_or(Value::Null),
                "message": projected.get("message").cloned().unwrap_or(Value::Null),
                "occurrences_count": projected.get("occurrences_count").cloned().unwrap_or(Value::Null),
                "coverage_state": coverage,
                "group_ref": group_ref.map_or(Value::Null, |g| json!(g)),
                "evidence": evidence,
            }));
            let added = insert_observation(
                con,
                occurrence_id,
                source_scan_id,
                &ObservationRow {
                    role: spec.role.clone(),
                    issue_ordinal: issue_ordinal.clone(),
                    evidence_state: evidence["state"].as_str().unwrap_or("").to_string(),
                    evidence_reason: evidence["reason"].as_str().unwrap_or("").to_string(),
                    evidence_json,
                    payload_sha256: sha256_hex(occurrence_payload.as_bytes()),
                    config_fingerprint: prepared
                        .scan
                        .get("config_fingerprint")
                        .cloned()
                        .unwrap_or(Value::Null),
                    producer_version: prepared
                        .audit_row
                        .get("analyzer_version")
                        .cloned()
                        .unwrap_or(Value::Null),
                    producer_revision: prepared
                        .audit_row
                        .get("analyzer_revision")
                        .cloned()
                        .unwrap_or(Value::Null),
                    observed_at: ctx.observed_at.clone(),
                    observed_at_state: if ctx.observed_at.is_some() {
                        "known"
                    } else {
                        "unknown"
                    }
                    .into(),
                    ingested_at: now.to_string(),
                },
                refuse_new,
            )?;
            *counts.get_mut("observations").unwrap_or(&mut 0) += i64::from(added);
        }
    }
    // Membership completeness is a derived reader result, not source identity. Persist the final
    // state after every issue reference has been checked, including a correction to a
    // previously ingested immutable source.
    let scope_changed = con.execute(
        "UPDATE source_scan SET group_memberships_state=? WHERE source_scan_id=? AND group_memberships_state!=?",
        params![group_memberships_state, source_scan_id, group_memberships_state],
    )?;
    let changed = binding.created
        || binding.restored
        || scope_changed > 0
        || counts.values().any(|c| *c != 0);
    if changed {
        con.execute(
            "UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1",
            [],
        )?;
    }
    let revision: i64 = con.query_row(
        "SELECT ledger_revision FROM ledger WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let mut recorded = Record::new();
    for (k, v) in &counts {
        recorded.insert((*k).to_string(), json!(v));
    }
    recorded.insert(
        "group_memberships_state".into(),
        json!(group_memberships_state),
    );
    let get = |m: &Record, k: &str| m.get(k).cloned().unwrap_or(Value::Null);
    let mut out = Record::new();
    out.insert("ok".into(), json!(true));
    out.insert("ledger_revision".into(), json!(revision));
    out.insert("site_id".into(), json!(site_id));
    out.insert("site_netloc".into(), json!(ctx.netloc));
    out.insert("source_scan_id".into(), json!(source_scan_id));
    out.insert("scan_uuid".into(), get(&prepared.scan, "scan_uuid"));
    out.insert(
        "format_version".into(),
        get(&prepared.scan, "format_version"),
    );
    out.insert(
        "evidence_revision".into(),
        get(&prepared.scan, "evidence_revision"),
    );
    out.insert("audit_sha256".into(), get(&prepared.audit_row, "sha256"));
    out.insert("new_source".into(), json!(binding.created));
    out.insert("already_recorded".into(), json!(!changed));
    out.insert("scan_bytes_differ".into(), json!(binding.bytes_differ));
    out.insert("recorded".into(), Value::Object(recorded));
    Ok(out)
}

/// Record that a bound source artifact is gone (pruned or moved away).
///
/// Only the availability flag changes: the binding, digests and observation history stay so
/// every recorded case still cites its original evidence.
pub fn note_source_missing(
    ledger: LedgerRef<'_>,
    source_scan_id: i64,
    reason: &str,
) -> Result<Record, StoreError> {
    if reason.is_empty() || reason.chars().count() > 512 {
        return Err(ledger_error(
            "a missing-source note requires a bounded reason",
        ));
    }
    let con = ledger.open(true)?;
    let state: Option<String> = con
        .query_row(
            "SELECT artifact_state FROM source_scan WHERE source_scan_id=?",
            [source_scan_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(state) = state else {
        return Err(ledger_error("unknown source scan binding"));
    };
    con.execute_batch("BEGIN IMMEDIATE")?;
    let run = || -> Result<i64, StoreError> {
        if state != "missing" {
            con.execute(
                "UPDATE source_scan SET artifact_state='missing',missing_reason=? WHERE source_scan_id=?",
                params![reason, source_scan_id],
            )?;
            con.execute(
                "UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1",
                [],
            )?;
        }
        Ok(con.query_row(
            "SELECT ledger_revision FROM ledger WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    };
    match run() {
        Ok(revision) => {
            con.execute_batch("COMMIT")?;
            let mut out = Record::new();
            out.insert("ok".into(), json!(true));
            out.insert("ledger_revision".into(), json!(revision));
            Ok(out)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}
