//! Disposable disk-backed BI lookups; never modify retained source artifacts.
//!
//! Private bounded SQLite scratch index under the output parent: finding-group lookups with
//! companion publication, quadrant candidate lookup, inlink share index, and primary segments.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::py;
use crate::segments::{self, Memberships, Segment, SegmentError};

use crate::bi::schema::{
    canonical_json, digest, f, format_cell, schema_fields, BIExportError, Field, Groups, RunInput,
    MAX_CELL_BYTES, MAX_FINDINGS, MAX_OUTPUT_PARTITIONS, MAX_PROJECTION_INDEX_BYTES,
    MIN_FREE_DISK_BYTES,
};

/// `INLINE_MEMBER_LIMIT`: groups at or under this size may inline members.
pub const INLINE_MEMBER_LIMIT: i64 = 25;
/// `INLINE_MEMBER_BYTES`: total JSON budget for inlined member rows.
pub const INLINE_MEMBER_BYTES: usize = 64 * 1024;

fn bi_err(msg: impl Into<String>) -> BIExportError {
    BIExportError::new(msg)
}

/// Map any projection-index SQLite failure to the bounded-index message.
pub fn map_sqlite(_: rusqlite::Error) -> BIExportError {
    bi_err("BI projection index exceeded its disk bound or became unavailable")
}

fn free_bytes(path: &Path) -> Option<u64> {
    spiderhead_core::filesystem::free_space(path).ok()
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn private_dir(parent: &Path, prefix: &str) -> Result<PathBuf, BIExportError> {
    for attempt in 0..100u32 {
        let name = format!(
            "{prefix}{}-{}-{attempt}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = parent.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(bi_err(format!("cannot create BI scratch directory: {e}"))),
        }
    }
    Err(bi_err("cannot create BI scratch directory"))
}

/// Projection index: bounds SQLite's main file, cache and temporary work in a private
/// directory that is removed on drop.
pub struct ProjectionIndex {
    /// Live scratch connection; closed before the directory is removed.
    pub con: Connection,
    dir: PathBuf,
}

impl Drop for ProjectionIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Open a projection index under `parent` with a byte budget.
pub fn projection_index(parent: &Path, max_bytes: u64) -> Result<ProjectionIndex, BIExportError> {
    let free = free_bytes(parent).unwrap_or(u64::MAX);
    if free < MIN_FREE_DISK_BYTES {
        return Err(bi_err("insufficient free disk for the BI projection index"));
    }
    let dir = private_dir(parent, ".seohead-bi-index-")?;
    let result = (|| -> Result<ProjectionIndex, BIExportError> {
        let con = Connection::open(dir.join("index.sqlite")).map_err(map_sqlite)?;
        con.execute_batch(&format!(
            "PRAGMA page_size=4096; PRAGMA max_page_count={}; PRAGMA cache_size=-2048; \
             PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;",
            max_bytes / 4096
        ))
        .map_err(map_sqlite)?;
        Ok(ProjectionIndex {
            con,
            dir: dir.clone(),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
}

/// Finding groups kept on disk; references are marked while findings stream.
pub struct GroupIndex<'a> {
    con: &'a Connection,
    /// Source group count ingested.
    pub source_count: usize,
    /// Companion identity bound to the run once the companion is published.
    pub reference: Map<String, Value>,
}

impl<'a> GroupIndex<'a> {
    /// Create the group index on a connection, over the given groups.
    pub fn new(con: &'a Connection, groups: &mut Groups) -> Result<Self, BIExportError> {
        let mut index = GroupIndex {
            con,
            source_count: 0,
            reference: Map::new(),
        };
        con.execute(
            "CREATE TABLE groups (id TEXT PRIMARY KEY, ordinal INTEGER, value TEXT, \
             member_count INTEGER, members_sha256 TEXT DEFAULT '', linked INTEGER DEFAULT 0)",
            [],
        )
        .map_err(map_sqlite)?;
        con.execute(
            "CREATE TABLE group_members (group_id TEXT, ordinal INTEGER, value_json TEXT, \
             PRIMARY KEY(group_id,ordinal))",
            [],
        )
        .map_err(map_sqlite)?;
        for group in groups.iter() {
            index.source_count += 1;
            if index.source_count > MAX_FINDINGS {
                return Err(bi_err("finding groups exceed the source row bound"));
            }
            if !group.is_object() {
                return Err(bi_err("finding group must be an object"));
            }
            let group_id = group
                .get("group_id")
                .filter(|v| py::truthy(Some(v)))
                .or_else(|| group.get("id"));
            let group_id = match group_id.and_then(Value::as_str) {
                Some(id) if !id.is_empty() => id.to_owned(),
                _ => return Err(bi_err("finding group has no stable identity")),
            };
            let urls = match group.get("urls") {
                Some(Value::Array(items)) => items.clone(),
                _ => return Err(bi_err("finding group has no ordered member array")),
            };
            let stripped: Map<String, Value> = group
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|(k, _)| k != "urls")
                .collect();
            let value = canonical_json(&Value::Object(stripped)).map_err(|e| bi_err(e.0))?;
            if value.len() as u64 > MAX_CELL_BYTES {
                return Err(bi_err("one finding group exceeds the BI cell bound"));
            }
            con.execute(
                "INSERT INTO groups(id,ordinal,value,member_count) VALUES (?,?,?,?)",
                rusqlite::params![
                    group_id,
                    index.source_count as i64 - 1,
                    value,
                    urls.len() as i64
                ],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    bi_err(format!(
                        "duplicate finding group id {}",
                        py::repr_str(&group_id)
                    ))
                }
                other => map_sqlite(other),
            })?;
            let mut digest_state = sha2::Sha256::new();
            use sha2::Digest;
            for (ordinal, url) in urls.iter().enumerate() {
                let member = canonical_json(url).map_err(|e| bi_err(e.0))?;
                if member.len() as u64 > MAX_CELL_BYTES {
                    return Err(bi_err("one finding group member exceeds the BI cell bound"));
                }
                con.execute(
                    "INSERT INTO group_members VALUES(?,?,?)",
                    rusqlite::params![group_id, ordinal as i64, member],
                )
                .map_err(map_sqlite)?;
                let encoded = member.as_bytes();
                digest_state.update((encoded.len() as u64).to_be_bytes());
                digest_state.update(encoded);
            }
            con.execute(
                "UPDATE groups SET members_sha256=? WHERE id=?",
                rusqlite::params![format!("{:x}", digest_state.finalize()), group_id],
            )
            .map_err(map_sqlite)?;
        }
        Ok(index)
    }

    /// Mark the group linked and return its value: a `urls` reference object or,
    /// for small groups, the inline member list.
    pub fn get(&self, key: &str) -> Result<Option<Value>, BIExportError> {
        let row: Option<(String, i64, String)> = self
            .con
            .query_row(
                "SELECT value,member_count,members_sha256 FROM groups WHERE id=?",
                rusqlite::params![key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        let Some((raw, member_count, members_sha256)) = row else {
            return Ok(None);
        };
        self.con
            .execute(
                "UPDATE groups SET linked=1 WHERE id=? AND linked=0",
                rusqlite::params![key],
            )
            .map_err(map_sqlite)?;
        let mut group = serde_json::from_str::<Map<String, Value>>(&raw).unwrap_or_default();
        group.insert("member_count".to_owned(), json!(member_count));
        let mut reference = Map::new();
        for (k, v) in &self.reference {
            reference.insert(k.clone(), v.clone());
        }
        reference.insert("schema".to_owned(), json!("bi-group-members.v1"));
        reference.insert("group_id".to_owned(), json!(key));
        reference.insert("member_count".to_owned(), json!(member_count));
        reference.insert("members_sha256".to_owned(), json!(members_sha256));
        group.insert("urls".to_owned(), Value::Object(reference));
        if member_count <= INLINE_MEMBER_LIMIT {
            let mut members: Vec<Value> = Vec::new();
            let mut size = 2usize;
            let mut broke = false;
            let mut stmt = self
                .con
                .prepare("SELECT value_json FROM group_members WHERE group_id=? ORDER BY ordinal")
                .map_err(map_sqlite)?;
            let rows = stmt
                .query_map(rusqlite::params![key], |r| r.get::<_, String>(0))
                .map_err(map_sqlite)?;
            for member in rows {
                let member = member.map_err(map_sqlite)?;
                size += member.len() + usize::from(!members.is_empty());
                if size > INLINE_MEMBER_BYTES {
                    broke = true;
                    break;
                }
                members.push(serde_json::from_str(&member).unwrap_or(Value::Null));
            }
            if !broke {
                group.insert("urls".to_owned(), Value::Array(members));
            }
        }
        Ok(Some(Value::Object(group)))
    }

    /// Ordered companion CSV rows for a run and kind.
    pub fn companion_rows(&self, run_id: &str, kind: &str) -> Result<Vec<Value>, BIExportError> {
        let mut out = Vec::new();
        if kind == "groups" {
            let mut stmt = self
                .con
                .prepare(
                    "SELECT id,ordinal,value,member_count,members_sha256 FROM groups \
                     ORDER BY ordinal",
                )
                .map_err(map_sqlite)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(map_sqlite)?;
            for row in rows {
                let (id, ordinal, raw, member_count, members_sha256) = row.map_err(map_sqlite)?;
                let group: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                out.push(json!({
                    "run_id": run_id,
                    "group_id": id,
                    "group_ordinal": ordinal,
                    "member_count": member_count,
                    "declared_count": group.get("count").cloned().unwrap_or(Value::Null),
                    "metadata_json": group,
                    "members_sha256": members_sha256,
                }));
            }
        } else {
            let mut stmt = self
                .con
                .prepare(
                    "SELECT m.group_id,m.ordinal,m.value_json FROM group_members m \
                     JOIN groups g ON g.id=m.group_id ORDER BY g.ordinal,m.ordinal",
                )
                .map_err(map_sqlite)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(map_sqlite)?;
            for row in rows {
                let (group_id, ordinal, raw) = row.map_err(map_sqlite)?;
                out.push(json!({
                    "run_id": run_id,
                    "group_id": group_id,
                    "member_ordinal": ordinal,
                    "member_json": serde_json::from_str(&raw).unwrap_or(Value::Null),
                }));
            }
        }
        Ok(out)
    }

    /// Group counts as `(total, linked)`.
    pub fn coverage(&self) -> Result<(i64, i64), BIExportError> {
        self.con
            .query_row(
                "SELECT COUNT(*),COALESCE(SUM(linked),0) FROM groups",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(map_sqlite)
    }

    /// Ordered ids of groups that no finding referenced.
    pub fn unlinked(&self) -> Result<Vec<String>, BIExportError> {
        let mut stmt = self
            .con
            .prepare("SELECT id FROM groups WHERE linked=0 ORDER BY id")
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sqlite)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_sqlite)?);
        }
        Ok(out)
    }
}

/// Field names of a finding group.
pub fn group_fields(kind: &str) -> Vec<Field> {
    let shared = [
        f("run_id", "string", false, ""),
        f("group_id", "string", false, ""),
    ];
    if kind == "groups" {
        return shared
            .into_iter()
            .chain([
                f("group_ordinal", "integer", false, ""),
                f("member_count", "integer", false, ""),
                f("declared_count", "integer", true, ""),
                f("metadata_json", "json", false, ""),
                f("members_sha256", "string", false, ""),
            ])
            .collect();
    }
    shared
        .into_iter()
        .chain([
            f("member_ordinal", "integer", false, ""),
            f("member_json", "json", false, ""),
        ])
        .collect()
}

/// Digest of companion content.
pub fn companion_digest(descriptor: &Value) -> Result<String, BIExportError> {
    let stripped: Map<String, Value> = py::map(descriptor)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| k != "sha256")
        .collect();
    Ok(digest(
        canonical_json(&Value::Object(stripped))
            .map_err(|e| bi_err(e.0))?
            .as_bytes(),
    ))
}

/// Write every group and member once to a companion file, separate from the six main datasets.
pub fn write_group_members_companion(
    index: &mut GroupIndex,
    directory: &Path,
    run_id: &str,
    source_audit_sha256: Option<&str>,
    max_rows_per_file: usize,
    max_bytes_per_file: u64,
    budget: &crate::bi::writer::OutputBudget,
) -> Result<Value, BIExportError> {
    let mut descriptor = json!({
        "format": "bi-group-members.v1",
        "run_id": run_id,
        "source_audit_sha256": source_audit_sha256,
        "inline_member_limit": INLINE_MEMBER_LIMIT,
        "inline_member_bytes": INLINE_MEMBER_BYTES,
        "publication": "local companion; not included in the six published datasets",
    });
    for kind in ["groups", "members"] {
        let fields = group_fields(kind);
        let mut writer = crate::bi::writer::PartitionWriter::new(
            directory,
            &format!("group-{kind}"),
            &fields,
            max_rows_per_file,
            max_bytes_per_file,
            budget,
        );
        let write_result = (|| -> Result<Value, BIExportError> {
            for row in index.companion_rows(run_id, kind)? {
                writer.write(&row)?;
            }
            writer.finish()
        })();
        writer.close();
        let mut result = write_result?;
        result
            .as_object_mut()
            .map(|m| m.insert("fields".to_owned(), Value::Array(schema_fields(&fields))));
        descriptor[kind] = result;
    }
    descriptor["sha256"] = json!(companion_digest(&descriptor)?);
    verify_group_members_companion(directory, &descriptor)?;
    index.reference.insert("run_id".to_owned(), json!(run_id));
    index.reference.insert(
        "source_audit_sha256".to_owned(),
        source_audit_sha256.map_or(Value::Null, |s| json!(s)),
    );
    index
        .reference
        .insert("companion_sha256".to_owned(), descriptor["sha256"].clone());
    Ok(descriptor)
}

/// Decode companion CSV rows keyed by their header names.
fn companion_rows_disk(
    root: &Path,
    descriptor: &Value,
    kind: &str,
) -> Result<Vec<Map<String, Value>>, BIExportError> {
    let mut out = Vec::new();
    for part in descriptor
        .get(kind)
        .and_then(|d| d.get("partitions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let path = root.join(part.get("path").and_then(Value::as_str).unwrap_or(""));
        let file =
            fs::File::open(&path).map_err(|_| bi_err("group companion has an invalid CSV row"))?;
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(file);
        let headers = reader
            .headers()
            .map_err(|_| bi_err("group companion has an invalid CSV row"))?
            .clone();
        for record in reader.records() {
            let record = record.map_err(|_| bi_err("group companion has an invalid CSV row"))?;
            if record.len() != headers.len() {
                return Err(bi_err("group companion has an invalid CSV row"));
            }
            let mut row = Map::new();
            for (name, value) in headers.iter().zip(record.iter()) {
                if value.len() as u64 > MAX_CELL_BYTES {
                    return Err(bi_err("group companion has an invalid CSV row"));
                }
                row.insert(name.to_owned(), json!(value));
            }
            out.push(row);
        }
    }
    Ok(out)
}

/// SHA-256, byte count and row count of one companion CSV.
fn group_partition_stats(
    path: &Path,
    fields: &[String],
) -> Result<(String, u64, u64), BIExportError> {
    use sha2::Digest;
    let mut digest_state = sha2::Sha256::new();
    let mut size = 0u64;
    let mut count = 0u64;
    let bytes = fs::read(path).map_err(|_| bi_err("group companion CSV is invalid"))?;
    digest_state.update(&bytes);
    size += bytes.len() as u64;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes.as_slice());
    let header = reader
        .headers()
        .map_err(|_| bi_err("group companion CSV is invalid"))?
        .clone();
    if header.iter().map(str::to_owned).collect::<Vec<_>>() != *fields {
        return Err(bi_err("group companion CSV header differs"));
    }
    for record in reader.records() {
        let record = record.map_err(|_| bi_err("group companion CSV is invalid"))?;
        if record.len() != fields.len()
            || record.iter().any(|cell| cell.len() as u64 > MAX_CELL_BYTES)
        {
            return Err(bi_err(
                "group companion row exceeds its cell or field bound",
            ));
        }
        count += 1;
    }
    Ok((format!("{:x}", digest_state.finalize()), size, count))
}

/// Validate ordered identities, exact counts, source binding and complete file hashes
/// of a group-members companion.
pub fn verify_group_members_companion(
    root: &Path,
    descriptor: &Value,
) -> Result<Option<Value>, BIExportError> {
    if descriptor.is_null() {
        return Ok(None);
    }
    if !descriptor.is_object()
        || descriptor.get("format").and_then(Value::as_str) != Some("bi-group-members.v1")
        || descriptor.get("sha256").and_then(Value::as_str)
            != Some(companion_digest(descriptor)?.as_str())
    {
        return Err(bi_err(
            "group companion identity or manifest digest differs",
        ));
    }
    let audit_sha = descriptor
        .get("source_audit_sha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    let audit_sha_ok = audit_sha.len() == 64
        && audit_sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if !descriptor.get("run_id").is_some_and(Value::is_string) || !audit_sha_ok {
        return Err(bi_err("group companion lacks its source audit binding"));
    }
    let mut files: BTreeSet<String> = BTreeSet::new();
    for kind in ["groups", "members"] {
        let data = descriptor.get(kind);
        let fields = schema_fields(&group_fields(kind));
        let partitions = data.and_then(|d| d.get("partitions"));
        if !data.is_some_and(Value::is_object)
            || data.and_then(|d| d.get("fields")) != Some(&Value::Array(fields.clone()))
            || !partitions.is_some_and(Value::is_array)
            || partitions
                .and_then(Value::as_array)
                .is_none_or(|p| p.is_empty() || p.len() > MAX_OUTPUT_PARTITIONS)
        {
            return Err(bi_err("group companion schema or partitions are invalid"));
        }
        let mut row_count = 0u64;
        let mut byte_count = 0u64;
        for part in partitions
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let relative = part.get("path").and_then(Value::as_str);
            let name_ok = relative.is_some_and(|rel| {
                regex::Regex::new(&format!(r"^group-{kind}-[0-9]{{4,}}\.csv$"))
                    .map(|re| re.is_match(rel))
                    .unwrap_or(false)
                    && !files.contains(rel)
            });
            let Some(relative) = relative.filter(|_| name_ok) else {
                return Err(bi_err("group companion partition path is invalid"));
            };
            files.insert(relative.to_owned());
            let path = root.join(relative);
            if path.is_symlink() || !path.is_file() {
                return Err(bi_err("group companion partition is unavailable"));
            }
            let field_names: Vec<String> = fields
                .iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect();
            let (digest, size, rows) = group_partition_stats(&path, &field_names)?;
            if (digest.as_str(), size, rows)
                != (
                    part.get("sha256").and_then(Value::as_str).unwrap_or(""),
                    part.get("bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX),
                    part.get("rows").and_then(Value::as_u64).unwrap_or(u64::MAX),
                )
            {
                return Err(bi_err("group companion partition digest or counts differ"));
            }
            row_count += rows;
            byte_count += size;
        }
        let data = data.cloned().unwrap_or(Value::Null);
        let totals_ok = row_count
            == data
                .get("row_count")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX)
            && byte_count
                == data
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX)
            && partitions
                .and_then(Value::as_array)
                .map(|p| p.len())
                .unwrap_or(0)
                == data
                    .get("partition_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX) as usize;
        if !totals_ok {
            return Err(bi_err("group companion totals differ"));
        }
    }
    let index = projection_index(root, MAX_PROJECTION_INDEX_BYTES)?;
    index
        .con
        .execute("CREATE TABLE seen_groups (id TEXT PRIMARY KEY)", [])
        .map_err(map_sqlite)?;
    let members = companion_rows_disk(root, descriptor, "members")?;
    let groups = companion_rows_disk(root, descriptor, "groups")?;
    let mut member_iter = members.iter().peekable();
    let mut consumed = 0u64;
    let mismatch = || bi_err("group companion members, order or identity differ");
    for (ordinal, group) in groups.iter().enumerate() {
        let result = (|| -> Result<(), BIExportError> {
            let metadata: Value = serde_json::from_str(
                group
                    .get("metadata_json")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            .map_err(|_| mismatch())?;
            let group_id = metadata
                .get("group_id")
                .filter(|v| py::truthy(Some(v)))
                .or_else(|| metadata.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(mismatch)?;
            if group_id.is_empty() {
                return Err(mismatch());
            }
            let count: i64 = group
                .get("member_count")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .parse()
                .map_err(|_| mismatch())?;
            let declared = match metadata.get("count") {
                None | Some(Value::Null) => String::new(),
                Some(v) => py::str_value(v),
            };
            if count < 0
                || group
                    .get("group_ordinal")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| mismatch())?
                    != ordinal as i64
                || group
                    .get("declared_count")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    != declared
            {
                return Err(mismatch());
            }
            let gid_value = json!(group_id);
            let (expected_gid, _) =
                format_cell(&gid_value, "string", "group_id").map_err(|_| mismatch())?;
            let run_value = descriptor.get("run_id").cloned().unwrap_or(Value::Null);
            let (expected_run, _) =
                format_cell(&run_value, "string", "run_id").map_err(|_| mismatch())?;
            if group.get("group_id").and_then(Value::as_str).unwrap_or("") != expected_gid
                || group.get("run_id").and_then(Value::as_str).unwrap_or("") != expected_run
            {
                return Err(mismatch());
            }
            index
                .con
                .execute(
                    "INSERT INTO seen_groups VALUES(?)",
                    rusqlite::params![group_id],
                )
                .map_err(|_| mismatch())?;
            use sha2::Digest;
            let mut digest_state = sha2::Sha256::new();
            for member_ordinal in 0..count {
                let member = member_iter.next().ok_or_else(mismatch)?;
                if member.get("group_id") != group.get("group_id")
                    || member.get("run_id") != group.get("run_id")
                    || member
                        .get("member_ordinal")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| mismatch())?
                        != member_ordinal
                {
                    return Err(mismatch());
                }
                let member_json: Value = serde_json::from_str(
                    member
                        .get("member_json")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
                .map_err(|_| mismatch())?;
                let encoded = canonical_json(&member_json)
                    .map_err(|_| mismatch())?
                    .into_bytes();
                digest_state.update((encoded.len() as u64).to_be_bytes());
                digest_state.update(encoded);
                consumed += 1;
            }
            if format!("{:x}", digest_state.finalize())
                != group
                    .get("members_sha256")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            {
                return Err(mismatch());
            }
            Ok(())
        })();
        result?;
    }
    if member_iter.next().is_some()
        || consumed
            != descriptor
                .get("members")
                .and_then(|m| m.get("row_count"))
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX)
    {
        return Err(bi_err("group companion contains orphan members"));
    }
    Ok(Some(json!({
        "groups": descriptor.get("groups").and_then(|g| g.get("row_count")).cloned().unwrap_or(Value::Null),
        "members": consumed,
        "sha256": descriptor.get("sha256").cloned().unwrap_or(Value::Null),
    })))
}

/// Preserve the immutable, complete companion with a selected findings package.
pub fn copy_group_members_companion(
    root: &Path,
    destination: &Path,
    descriptor: &Value,
    budget: &crate::bi::writer::OutputBudget,
) -> Result<Option<Value>, BIExportError> {
    if descriptor.is_null() {
        return Ok(None);
    }
    verify_group_members_companion(root, descriptor)?;
    for kind in ["groups", "members"] {
        for part in descriptor
            .get(kind)
            .and_then(|d| d.get("partitions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let bytes = part.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            budget.reserve_bytes(bytes)?;
            budget.reserve_partition()?;
            let name = part.get("path").and_then(Value::as_str).unwrap_or("");
            let source = fs::read(root.join(name))
                .map_err(|e| bi_err(format!("group companion partition is unavailable: {e}")))?;
            let target = destination.join(name);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| bi_err(format!("group companion partition is unavailable: {e}")))?;
            use std::io::Write;
            file.write_all(&source)
                .map_err(|e| bi_err(format!("group companion partition is unavailable: {e}")))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
            }
        }
    }
    verify_group_members_companion(destination, descriptor)?;
    Ok(Some(descriptor.clone()))
}

/// Bind every versioned finding reference to the exact complete local companion.
pub fn verify_group_member_references(
    root: &Path,
    manifest: &Value,
) -> Result<Option<Value>, BIExportError> {
    let descriptor = manifest
        .get("group_members")
        .cloned()
        .unwrap_or(Value::Null);
    let result = verify_group_members_companion(root, &descriptor)?;
    let run = manifest.get("run").cloned().unwrap_or_else(|| json!({}));
    if !descriptor.is_null()
        && (descriptor.get("run_id") != run.get("run_id")
            || descriptor.get("source_audit_sha256") != run.get("audit_sha256"))
    {
        return Err(bi_err(
            "group companion does not bind to this package audit",
        ));
    }
    let index = projection_index(root, MAX_PROJECTION_INDEX_BYTES)?;
    index
        .con
        .execute(
            "CREATE TABLE member_refs (id TEXT PRIMARY KEY, count INTEGER, sha256 TEXT)",
            [],
        )
        .map_err(map_sqlite)?;
    if !descriptor.is_null() {
        for row in companion_rows_disk(root, &descriptor, "groups")? {
            let metadata: Value = serde_json::from_str(
                row.get("metadata_json")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            .unwrap_or(Value::Null);
            let group_id = metadata
                .get("group_id")
                .filter(|v| py::truthy(Some(v)))
                .or_else(|| metadata.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let count: i64 = row
                .get("member_count")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .parse()
                .map_err(|_| bi_err("group companion members, order or identity differ"))?;
            index
                .con
                .execute(
                    "INSERT INTO member_refs VALUES(?,?,?)",
                    rusqlite::params![
                        group_id,
                        count,
                        row.get("members_sha256")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ],
                )
                .map_err(map_sqlite)?;
        }
    }
    let dataset = manifest
        .get("datasets")
        .and_then(|d| d.get("findings"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    for part in dataset
        .get("partitions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let relative = part.get("path").and_then(Value::as_str);
        let valid = relative.is_some_and(|rel| {
            root.join(rel).parent() == Some(root) && !root.join(rel).is_symlink()
        });
        if !valid {
            return Err(bi_err("finding reference partition is invalid"));
        }
        let file = fs::File::open(root.join(relative.unwrap_or_default()))
            .map_err(|e| bi_err(format!("finding reference partition is invalid: {e}")))?;
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(file);
        let headers = reader
            .headers()
            .map_err(|e| bi_err(format!("CSV partition cannot be read: {e}")))?
            .clone();
        for record in reader.records() {
            let record =
                record.map_err(|e| bi_err(format!("CSV partition cannot be read: {e}")))?;
            let mut row = Map::new();
            for (name, value) in headers.iter().zip(record.iter()) {
                row.insert(name.to_owned(), json!(value));
            }
            let raw = row
                .get("group_urls_json")
                .and_then(Value::as_str)
                .unwrap_or("");
            if raw.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
            if !value.is_object() {
                continue;
            }
            if descriptor.is_null()
                || value.get("schema").and_then(Value::as_str) != Some("bi-group-members.v1")
            {
                return Err(bi_err(
                    "finding group reference lacks its complete companion",
                ));
            }
            let member_count = value.get("member_count");
            if !value.get("group_id").is_some_and(Value::is_string)
                || !member_count.is_some_and(py::is_int)
            {
                return Err(bi_err(
                    "finding group reference has invalid identity or count",
                ));
            }
            let member_count_i = member_count.and_then(Value::as_i64).unwrap_or(0);
            for (name, expected_value) in [
                (
                    "group_id",
                    format_cell(
                        &value.get("group_id").cloned().unwrap_or(Value::Null),
                        "string",
                        "group_id",
                    )?
                    .0,
                ),
                (
                    "run_id",
                    format_cell(
                        &value.get("run_id").cloned().unwrap_or(Value::Null),
                        "string",
                        "run_id",
                    )?
                    .0,
                ),
                ("group_url_count", member_count_i.to_string()),
            ] {
                if let Some(cell) = row.get(name).and_then(Value::as_str) {
                    if cell != expected_value {
                        return Err(bi_err(
                            "finding row differs from its group member reference",
                        ));
                    }
                }
            }
            let group_id = value.get("group_id").and_then(Value::as_str).unwrap_or("");
            let expected: Option<(i64, String)> = index
                .con
                .query_row(
                    "SELECT count,sha256 FROM member_refs WHERE id=?",
                    rusqlite::params![group_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            let members_sha = value.get("members_sha256").and_then(Value::as_str);
            let ok = expected.as_ref().is_some_and(|(count, sha)| {
                *count == member_count_i && Some(sha.as_str()) == members_sha
            }) && value.get("companion_sha256") == descriptor.get("sha256")
                && value.get("source_audit_sha256") == descriptor.get("source_audit_sha256")
                && value.get("run_id") == descriptor.get("run_id");
            if !ok {
                return Err(bi_err(
                    "finding group reference differs from its source companion",
                ));
            }
        }
    }
    Ok(result)
}

/// Fetch one candidate pair or its refusal reason, without a URL dictionary.
pub struct QuadrantLookup<'a> {
    con: &'a Connection,
    blocked: bool,
}

/// One quadrant `get` outcome.
pub enum QuadrantHit {
    /// No refusal and no complete pair.
    Miss,
    /// Refusal reason recorded for the key.
    Reason(String),
    /// `(search, sessions)` decoded source observations.
    Pair(Value, Value),
}

impl<'a> QuadrantLookup<'a> {
    /// Create a lookup over a connection with the blocked-URL set.
    pub fn new(con: &'a Connection, blocked: bool) -> Self {
        QuadrantLookup { con, blocked }
    }

    /// Look up one URL and return its `QuadrantHit`, or the default when absent.
    pub fn get(&self, url: &str) -> Result<QuadrantHit, BIExportError> {
        let refused: Option<String> = self
            .con
            .query_row(
                "SELECT reason FROM blocked WHERE url=?",
                rusqlite::params![url],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        if self.blocked {
            return Ok(match refused {
                Some(reason) => QuadrantHit::Reason(reason),
                None => QuadrantHit::Miss,
            });
        }
        if refused.is_some() {
            return Ok(QuadrantHit::Miss);
        }
        let row: Option<(String, String)> = self
            .con
            .query_row(
                "SELECT search_json,sessions_json FROM candidates \
                 WHERE url=? AND search_count=1 AND sessions_count=1 LIMIT 1",
                rusqlite::params![url],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        Ok(match row {
            Some((search, sessions)) => QuadrantHit::Pair(
                serde_json::from_str(&search).unwrap_or(Value::Null),
                serde_json::from_str(&sessions).unwrap_or(Value::Null),
            ),
            None => QuadrantHit::Miss,
        })
    }
}

/// Count unique eligible source pages, independently of link occurrences.
pub struct InlinkIndex<'a> {
    con: &'a Connection,
    /// Denominator: pages with completed HTML link extraction.
    pub denominator: i64,
}

impl<'a> InlinkIndex<'a> {
    /// Create an inlink index for one run.
    pub fn new(con: &'a Connection, run: &mut RunInput) -> Result<Self, BIExportError> {
        con.execute(
            "CREATE TABLE eligible (url TEXT PRIMARY KEY, complete INTEGER)",
            [],
        )
        .map_err(map_sqlite)?;
        for page in (run.pages_factory)() {
            use unicode_casefold::UnicodeCaseFold;
            let media = {
                let raw = page.get("content_type");
                let text = if py::truthy(raw) {
                    py::str_value(raw.unwrap_or(&Value::Null))
                } else {
                    String::new()
                };
                let folded: String = text
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .case_fold()
                    .collect();
                folded
            };
            let complete = page.get("document_id").is_some_and(|v| !v.is_null())
                && !py::truthy(page.get("body_unavailable"))
                && (media == "text/html" || media == "application/xhtml+xml");
            let url = page
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            con.execute(
                "INSERT INTO eligible VALUES (?,?)",
                rusqlite::params![url, i64::from(complete)],
            )
            .map_err(map_sqlite)?;
        }
        let denominator: i64 = con
            .query_row("SELECT COUNT(*) FROM eligible WHERE complete=1", [], |r| {
                r.get(0)
            })
            .map_err(map_sqlite)?;
        con.execute(
            "CREATE TABLE inlinks (target TEXT, source TEXT, PRIMARY KEY(target,source))",
            [],
        )
        .map_err(map_sqlite)?;
        for link in (run.links_factory)() {
            let destination = link
                .get("destination_url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let source = link.get("source_url").and_then(Value::as_str).unwrap_or("");
            con.execute(
                "INSERT OR IGNORE INTO inlinks SELECT ?,? \
                 WHERE EXISTS(SELECT 1 FROM eligible WHERE url=? AND complete=1) \
                 AND EXISTS(SELECT 1 FROM eligible WHERE url=?)",
                rusqlite::params![destination, source, source, destination],
            )
            .map_err(map_sqlite)?;
        }
        Ok(InlinkIndex { con, denominator })
    }

    /// Unique retained eligible sources linking to `url`.
    pub fn numerator(&self, url: &str) -> Result<i64, BIExportError> {
        self.con
            .query_row(
                "SELECT COUNT(*) FROM inlinks WHERE target=?",
                rusqlite::params![url],
                |r| r.get(0),
            )
            .map_err(map_sqlite)
    }
}

fn urlsplit_parts(url: &str) -> (String, String) {
    match url::Url::parse(url) {
        Ok(parsed) => (
            parsed.path().to_owned(),
            parsed.host_str().unwrap_or("").to_lowercase(),
        ),
        Err(_) => (url.to_owned(), String::new()),
    }
}

/// Dotted-path presence on the raw record. No projected fallback: presence is about the retained
/// source, not the projected value.
fn field_present(record: &Value, field: &str) -> bool {
    let mut value = record;
    for part in field.split('.') {
        match value.get(part) {
            Some(next) => value = next,
            None => return false,
        }
    }
    true
}

/// Membership lookup in SQLite, shaped like a map, for the shared rule evaluator.
pub struct SegmentMemberships<'a> {
    con: &'a Connection,
}

impl<'a> Memberships for SegmentMemberships<'a> {
    fn contains(&self, name: &str, url: &str) -> bool {
        self.con
            .query_row(
                "SELECT 1 FROM segment_memberships WHERE segment=? AND url=?",
                rusqlite::params![name, url],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    fn unknown_fields(&self, name: &str, url: &str) -> BTreeSet<String> {
        self.con
            .query_row(
                "SELECT fields_json FROM segment_unknown WHERE segment=? AND url=?",
                rusqlite::params![name, url],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default()
    }
}

/// Evaluate shared OR rules while preserving unavailable retained fields.
fn segment_state(
    segment: &Segment,
    record: &Value,
    memberships: &SegmentMemberships,
    required: &BTreeSet<String>,
) -> Result<(String, BTreeSet<String>), SegmentError> {
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let url = record.get("url").and_then(Value::as_str).unwrap_or("");
    for rule in &segment.rules {
        if rule.op == "segment" {
            let name = py::str_value(&rule.value);
            if memberships.contains(&name, url) {
                return Ok(("matched".to_owned(), BTreeSet::new()));
            }
            unknown.extend(memberships.unknown_fields(&name, url));
            continue;
        }
        if !field_present(record, &rule.field) {
            unknown.insert(rule.field.clone());
            continue;
        }
        if segments::rule_matches(rule, record, memberships)? {
            return Ok(("matched".to_owned(), BTreeSet::new()));
        }
    }
    unknown.retain(|f| required.contains(f));
    if unknown.is_empty() {
        Ok(("not_matched".to_owned(), BTreeSet::new()))
    } else {
        Ok(("unknown".to_owned(), unknown))
    }
}

/// Disk-backed primary segments, evaluated with the shared segment rule engine.
pub struct PrimarySegmentIndex<'a> {
    con: &'a Connection,
    /// Resolved segment definitions in dependency order.
    pub order: Vec<Segment>,
    required: std::collections::HashMap<String, BTreeSet<String>>,
    order_index: std::collections::HashMap<String, usize>,
}

impl<'a> PrimarySegmentIndex<'a> {
    /// Create the index over a connection and resolved segment definitions.
    pub fn new(con: &'a Connection, definitions: &[Value]) -> Result<Self, BIExportError> {
        let order = segments::resolve_order(definitions).map_err(|e| bi_err(e.0))?;
        let required = segments::required_fields_ordered(&order)
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();
        let order_index = order
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();
        con.execute(
            "CREATE TABLE segment_pages (url TEXT PRIMARY KEY, record_json TEXT NOT NULL)",
            [],
        )
        .map_err(map_sqlite)?;
        con.execute(
            "CREATE TABLE segment_memberships (segment TEXT NOT NULL, url TEXT NOT NULL, \
             PRIMARY KEY(segment,url))",
            [],
        )
        .map_err(map_sqlite)?;
        con.execute(
            "CREATE TABLE primary_segments (url TEXT PRIMARY KEY, segment TEXT NOT NULL)",
            [],
        )
        .map_err(map_sqlite)?;
        con.execute(
            "CREATE TABLE segment_unknown (segment TEXT NOT NULL, url TEXT NOT NULL, \
             fields_json TEXT NOT NULL, PRIMARY KEY(segment,url))",
            [],
        )
        .map_err(map_sqlite)?;
        Ok(PrimarySegmentIndex {
            con,
            order,
            required,
            order_index,
        })
    }

    /// Retain page rows, then evaluate each segment in order.
    pub fn build(&self, pages: impl IntoIterator<Item = Value>) -> Result<(), BIExportError> {
        for page in pages {
            let url = match page.get("url").and_then(Value::as_str) {
                Some(u) if !u.is_empty() => u.to_owned(),
                _ => continue,
            };
            let mut record = page.as_object().cloned().unwrap_or_default();
            let (path, host) = urlsplit_parts(&url);
            record.insert("url".to_owned(), json!(url));
            record.entry("path".to_owned()).or_insert(json!(path));
            record.entry("host".to_owned()).or_insert(json!(host));
            let raw = canonical_json(&Value::Object(record)).map_err(|e| bi_err(e.0))?;
            self.con
                .execute(
                    "INSERT INTO segment_pages(url,record_json) VALUES (?,?)",
                    rusqlite::params![url, raw],
                )
                .map_err(map_sqlite)?;
        }
        let memberships = SegmentMemberships { con: self.con };
        for segment in &self.order {
            let mut stmt = self
                .con
                .prepare("SELECT url,record_json FROM segment_pages ORDER BY url")
                .map_err(map_sqlite)?;
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(map_sqlite)?
                .collect::<Result<_, _>>()
                .map_err(map_sqlite)?;
            for (url, raw) in rows {
                let record: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let empty: BTreeSet<String> = BTreeSet::new();
                let required = self.required.get(&segment.name).unwrap_or(&empty);
                let (state, missing) = segment_state(segment, &record, &memberships, required)
                    .map_err(|e| bi_err(e.0))?;
                match state.as_str() {
                    "matched" => {
                        self.con
                            .execute(
                                "INSERT INTO segment_memberships(segment,url) VALUES (?,?)",
                                rusqlite::params![segment.name, url],
                            )
                            .map_err(map_sqlite)?;
                        self.con
                            .execute(
                                "INSERT OR IGNORE INTO primary_segments(url,segment) VALUES (?,?)",
                                rusqlite::params![url, segment.name],
                            )
                            .map_err(map_sqlite)?;
                    }
                    "unknown" => {
                        self.con
                            .execute(
                                "INSERT INTO segment_unknown(segment,url,fields_json) VALUES (?,?,?)",
                                rusqlite::params![
                                    segment.name,
                                    url,
                                    serde_json::to_string(
                                        &missing.iter().collect::<Vec<_>>()
                                    )
                                    .unwrap_or_else(|_| "[]".to_owned())
                                ],
                            )
                            .map_err(map_sqlite)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The first matching first-order segment, or a derived isolated answer.
    /// Missing retained fields make the answer unavailable.
    pub fn primary(&self, url: &str) -> Result<String, BIExportError> {
        let row: Option<String> = self
            .con
            .query_row(
                "SELECT segment FROM primary_segments WHERE url=?",
                rusqlite::params![url],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some(segment) = row {
            let primary_index = self.order_index.get(&segment).copied().unwrap_or(0);
            let mut missing: BTreeSet<String> = BTreeSet::new();
            for (name, index) in &self.order_index {
                if *index >= primary_index {
                    continue;
                }
                let unknown: Option<String> = self
                    .con
                    .query_row(
                        "SELECT fields_json FROM segment_unknown WHERE url=? AND segment=?",
                        rusqlite::params![url, name],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(map_sqlite)?;
                if let Some(raw) = unknown {
                    if let Ok(fields) = serde_json::from_str::<Vec<String>>(&raw) {
                        missing.extend(fields);
                    }
                }
            }
            if !missing.is_empty() {
                return Err(bi_err(format!(
                    "segment selection is unavailable because retained page fields are missing: {}",
                    missing.iter().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
            return Ok(segment);
        }
        let mut stmt = self
            .con
            .prepare("SELECT fields_json FROM segment_unknown WHERE url=? ORDER BY segment")
            .map_err(map_sqlite)?;
        let rows: Vec<String> = stmt
            .query_map(rusqlite::params![url], |r| r.get(0))
            .map_err(map_sqlite)?
            .collect::<Result<_, _>>()
            .map_err(map_sqlite)?;
        if !rows.is_empty() {
            let mut missing: BTreeSet<String> = BTreeSet::new();
            for raw in rows {
                if let Ok(fields) = serde_json::from_str::<Vec<String>>(&raw) {
                    missing.extend(fields);
                }
            }
            return Err(bi_err(format!(
                "segment selection is unavailable because retained page fields are missing: {}",
                missing.iter().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let (path, host) = urlsplit_parts(url);
        let isolated = segments::assign_segments_ordered(
            &[json!({"url": url, "path": path, "host": host})],
            &self.order,
        )
        .map_err(|e| bi_err(e.0))?;
        let value = isolated
            .get("primary")
            .and_then(|p| p.get(url))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(match value.as_str() {
            None | Some(segments::UNSEGMENTED) => "default".to_owned(),
            Some(name) => name.to_owned(),
        })
    }
}
