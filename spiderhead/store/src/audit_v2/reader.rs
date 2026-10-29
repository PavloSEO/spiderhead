//! Validated, re-iterable access to an audit.v2 companion (`AuditV2Reader`).

use super::pyjson::dumps;
use super::{
    audit_v2_path, bind_to_scan, canonical, collection_markers, contains_group_marker, digest_item,
    digest_start, fail, get_pointer, AuditReadTimeout, APPLICATION_ID, COLLECTION_MARKER,
    EXPECTED_SCHEMA, FORMAT_VERSION, GROUP_MEMBERS_MARKER, MAX_HEADER_BYTES, MAX_ITEM_BYTES,
    USER_VERSION, V2_SCHEMA,
};
use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Ordered disk-backed group members; iteration never builds the full URL list for the caller.
pub struct AuditGroupMembers<'a> {
    reader: &'a AuditV2Reader,
    group_ordinal: i64,
    count: i64,
}

impl<'a> AuditGroupMembers<'a> {
    /// Members of `/groups[group_ordinal]`, `count` of them.
    pub fn new(reader: &'a AuditV2Reader, group_ordinal: i64, count: i64) -> Self {
        Self {
            reader,
            group_ordinal,
            count,
        }
    }

    /// Number of members.
    pub fn len(&self) -> i64 {
        self.count
    }

    /// Whether the group has no members.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Every member in order (`__iter__`).
    pub fn to_vec(&self) -> Result<Vec<Value>, StoreError> {
        self.reader.iter_group_members(self.group_ordinal, 0, None)
    }

    /// One member; negative indexes count from the end.
    pub fn get(&self, index: i64) -> Result<Option<Value>, StoreError> {
        let ordinal = if index >= 0 {
            index
        } else {
            self.count + index
        };
        if !(0..self.count).contains(&ordinal) {
            return Ok(None);
        }
        Ok(self
            .reader
            .iter_group_members(self.group_ordinal, ordinal, Some(1))?
            .into_iter()
            .next())
    }

    /// A forward page `[start, stop)` of at most 1000 members.
    pub fn slice(&self, start: i64, stop: i64) -> Result<Vec<Value>, StoreError> {
        let (start, stop) = (start.clamp(0, self.count), stop.clamp(0, self.count));
        if stop - start > 1000 {
            return Err(fail(
                "group member slices require a forward page of at most 1000",
            ));
        }
        if stop <= start {
            return Ok(Vec::new());
        }
        let page = self.reader.group_members_page(
            self.group_ordinal,
            start,
            (stop - start).max(1),
            1024 * 1024,
        )?;
        if page["returned"].as_i64() != Some(stop - start) {
            return Err(fail(
                "group slice exceeds its byte bound; use group_members_page",
            ));
        }
        Ok(page["rows"].as_array().cloned().unwrap_or_default())
    }
}

/// Validated, re-iterable access to an audit.v2 companion.
pub struct AuditV2Reader {
    deadline: Option<Instant>,
    /// Whether the scan binding is checked with the light scan opener.
    pub light_binding: bool,
    /// Absolute path of the native scan.
    pub scan_path: PathBuf,
    /// Path of the companion file.
    pub path: PathBuf,
    pub(crate) con: Connection,
    /// Storage revision found in the file (2 or 3).
    pub storage_version: i64,
    /// Parsed `binding_json`.
    pub binding: Value,
    /// Parsed `header_json` (collection arrays replaced by markers).
    pub header: Value,
    /// Stored content hash.
    pub sha256: String,
    /// Collection pointer -> row count.
    pub collections: BTreeMap<String, i64>,
}

fn is_group_marker(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|m| m.contains_key(GROUP_MEMBERS_MARKER))
}

impl AuditV2Reader {
    /// Open and fully validate the companion of `scan_path`.
    pub fn new(
        scan_path: &Path,
        verify_binding: bool,
        light_binding: bool,
        timeout_seconds: Option<f64>,
    ) -> Result<Self, StoreError> {
        if timeout_seconds.is_some_and(|t| !t.is_finite() || t <= 0.0) {
            return Err(StoreError::Invalid(
                "audit.v2 read timeout must be a finite positive number".into(),
            ));
        }
        let deadline = timeout_seconds
            .map(|t| {
                Duration::try_from_secs_f64(t)
                    .ok()
                    .and_then(|d| Instant::now().checked_add(d))
                    .ok_or_else(|| {
                        StoreError::Invalid(
                            "audit.v2 read timeout must be a finite positive number".into(),
                        )
                    })
            })
            .transpose()?;
        let scan_path = std::path::absolute(scan_path)?;
        let path = audit_v2_path(&scan_path);
        let info = std::fs::symlink_metadata(&path)
            .map_err(|e| fail(format!("audit.v2 companion is unavailable: {e}")))?;
        if !info.is_file() || nlink(&info) != 1 {
            return Err(fail("audit.v2 companion must be a regular unaliased file"));
        }
        let con = spiderhead_core::sqlite::open_readonly(
            &path,
            Duration::from_secs(5),
            &[
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA cache_size=-8192",
                "PRAGMA temp_store=FILE",
            ],
        )?;
        if let Some(limit) = deadline {
            con.progress_handler(1000, Some(move || Instant::now() >= limit));
        }
        let mut reader = Self {
            deadline,
            light_binding,
            scan_path,
            path,
            con,
            storage_version: 0,
            binding: Value::Null,
            header: Value::Null,
            sha256: String::new(),
            collections: BTreeMap::new(),
        };
        let outcome = reader
            .con
            .execute_batch("BEGIN")
            .map_err(StoreError::from)
            .and_then(|()| reader.validate(verify_binding));
        if let Err(error) = outcome {
            reader.check_read_budget()?;
            return Err(error);
        }
        Ok(reader)
    }

    /// Fail with `AuditReadTimeout` once the explicit read deadline has passed.
    pub fn check_read_budget(&self) -> Result<(), StoreError> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(StoreError::AuditReadTimeout(AuditReadTimeout {
                message: "audit.v2 read exceeded its explicit query budget".into(),
            }));
        }
        Ok(())
    }

    fn scalar(&self, sql: &str) -> Result<Option<i64>, StoreError> {
        Ok(self.con.query_row(sql, [], |r| r.get(0)).optional()?)
    }

    /// Validate identity, schema, integrity, ordering, markers and the content hash.
    pub fn validate(&mut self, verify_binding: bool) -> Result<(), StoreError> {
        self.check_read_budget()?;
        self.storage_version = self
            .con
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let app_id: i64 = self
            .con
            .query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if app_id != APPLICATION_ID || ![2, USER_VERSION].contains(&self.storage_version) {
            return Err(fail("unsupported audit.v2 companion identity or version"));
        }
        let objects: Vec<Vec<String>> = {
            let mut stmt = self.con.prepare(
                "SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
            )?;
            let rows = stmt.query_map([], |r| Ok(vec![r.get(0)?, r.get(1)?, r.get(2)?]))?;
            rows.collect::<Result<_, _>>()?
        };
        let expected = if self.storage_version == 2 {
            V2_SCHEMA
        } else {
            EXPECTED_SCHEMA
        };
        if objects.len() != expected.len()
            || !objects
                .iter()
                .zip(expected)
                .all(|(a, b)| a.iter().eq(b.iter()))
        {
            return Err(fail("audit.v2 companion schema differs"));
        }
        let quick: String = self.con.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if quick != "ok" {
            return Err(fail("audit.v2 companion failed SQLite quick_check"));
        }
        if self.con.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return Err(fail("audit.v2 companion has inconsistent foreign keys"));
        }
        if self
            .scalar("SELECT 1 FROM items WHERE typeof(ordinal)!='integer' LIMIT 1")?
            .is_some()
        {
            return Err(fail("audit.v2 collection ordinals must be integers"));
        }
        if self.storage_version == 3
            && self
                .scalar("SELECT 1 FROM group_members WHERE typeof(ordinal)!='integer' OR typeof(group_ordinal)!='integer' LIMIT 1")?
                .is_some()
        {
            return Err(fail("audit.v2 group member ordinals must be integers"));
        }
        let meta: Option<(String, String, String, String)> = self
            .con
            .query_row(
                "SELECT format_version,binding_json,header_json,sha256 FROM audit_meta WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((format, binding_json, header_json, sha256)) =
            meta.filter(|m| m.0 == FORMAT_VERSION)
        else {
            return Err(fail("audit.v2 companion has no matching format header"));
        };
        if self.scalar("SELECT COUNT(*) FROM audit_meta")? != Some(1) {
            return Err(fail(
                "audit.v2 companion must contain exactly one metadata row",
            ));
        }
        let _ = format;
        self.binding = serde_json::from_str(&binding_json)?;
        self.header = serde_json::from_str(&header_json)?;
        self.sha256 = sha256;
        if !self.binding.is_object() || !self.header.is_object() {
            return Err(fail("audit.v2 metadata must be JSON objects"));
        }
        if header_json.len() as i64 > MAX_HEADER_BYTES {
            return Err(fail("audit.v2 header exceeds its explicit 64 MiB limit"));
        }
        self.validate_collections()?;
        let digest = self.validate_rows(&binding_json, &header_json)?;
        if digest != self.sha256 {
            return Err(fail("audit.v2 content hash does not match"));
        }
        if verify_binding {
            let actual = bind_to_scan(
                &self.scan_path,
                self.binding.as_object().unwrap_or(&Map::new()),
                self.light_binding,
            )?;
            if Value::Object(actual) != self.binding {
                return Err(fail("audit.v2 binding disagrees with the native scan"));
            }
        }
        Ok(())
    }

    fn validate_collections(&mut self) -> Result<(), StoreError> {
        let listed: Vec<(String, rusqlite::types::Value)> = {
            let mut stmt = self
                .con
                .prepare("SELECT pointer,item_count FROM collections ORDER BY pointer")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (pointer, count) in listed {
            let rusqlite::types::Value::Integer(count) = count else {
                return Err(fail("audit.v2 has an invalid collection count"));
            };
            if count < 0 {
                return Err(fail("audit.v2 has an invalid collection count"));
            }
            let mut want = Map::new();
            want.insert(COLLECTION_MARKER.into(), Value::from(pointer.clone()));
            want.insert("count".into(), Value::from(count));
            if get_pointer(&self.header, &pointer)? != &Value::Object(want) {
                return Err(fail(format!(
                    "audit.v2 header and collection index disagree: {pointer}"
                )));
            }
            let (actual, low, high): (i64, Option<i64>, Option<i64>) = self.con.query_row(
                "SELECT COUNT(*),MIN(ordinal),MAX(ordinal) FROM items WHERE pointer=?",
                [&pointer],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if actual != count || (count != 0 && (low, high) != (Some(0), Some(count - 1))) {
                return Err(fail(format!(
                    "audit.v2 rows are incomplete or unordered: {pointer}"
                )));
            }
            self.collections.insert(pointer, count);
        }
        if self.collections.is_empty() {
            return Err(fail("audit.v2 companion has no collections"));
        }
        let have: BTreeSet<String> = self.collections.keys().cloned().collect();
        if collection_markers(&self.header) != have {
            return Err(fail(
                "audit.v2 header and collection index have different paths",
            ));
        }
        Ok(())
    }

    /// Walk every row and member, checking markers; returns the recomputed content hash.
    fn validate_rows(&self, binding_json: &str, header_json: &str) -> Result<String, StoreError> {
        let mut digest = digest_start(binding_json, header_json, self.storage_version);
        let v3 = self.storage_version == 3;
        let mut stmt = self
            .con
            .prepare("SELECT pointer,ordinal,value_json FROM items ORDER BY pointer,ordinal")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            self.check_read_budget()?;
            let (pointer, ordinal, raw): (String, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let value: Value =
                serde_json::from_str(&raw).map_err(|_| fail("audit.v2 row is invalid JSON"))?;
            if raw.len() as i64 > MAX_ITEM_BYTES {
                return Err(fail("audit.v2 row exceeds 8 MiB"));
            }
            if v3 && contains_group_marker(&value) {
                let urls_ok = value.get("urls").is_some_and(Value::is_object);
                let rest_clean = value.as_object().is_some_and(|m| {
                    !m.iter()
                        .any(|(k, v)| k != "urls" && contains_group_marker(v))
                        && !m.contains_key(GROUP_MEMBERS_MARKER)
                });
                if pointer != "/groups" || !urls_ok || !rest_clean {
                    return Err(fail(
                        "audit.v2 group marker occurs outside its member array",
                    ));
                }
            }
            if v3 && pointer == "/groups" {
                self.validate_group_marker(&value, ordinal)?;
            }
            digest_item(&mut digest, &pointer, ordinal, &raw);
        }
        if v3 {
            let mut stmt = self.con.prepare(
                "SELECT group_ordinal,ordinal,value_json FROM group_members ORDER BY group_ordinal,ordinal",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                self.check_read_budget()?;
                let (group, ordinal, raw): (i64, i64, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|_| fail("audit.v2 group member is invalid JSON"))?;
                if raw.len() as i64 > MAX_ITEM_BYTES {
                    return Err(fail("audit.v2 group member exceeds 8 MiB"));
                }
                if contains_group_marker(&value) {
                    return Err(fail("audit.v2 group member contains a reserved marker"));
                }
                digest_item(
                    &mut digest,
                    &format!("@group-members/{group}"),
                    ordinal,
                    &raw,
                );
            }
        }
        Ok(digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    fn validate_group_marker(&self, value: &Value, ordinal: i64) -> Result<(), StoreError> {
        let marker = value.get("urls").filter(|m| is_group_marker(m));
        let Some(Value::Object(marker)) = marker else {
            let lacks: bool = self
                .con
                .prepare("SELECT 1 FROM group_members WHERE group_ordinal=? LIMIT 1")?
                .exists([ordinal])?;
            if lacks {
                return Err(fail("audit.v2 group members lack their marker"));
            }
            return Ok(());
        };
        let count = marker.get("count").and_then(Value::as_i64);
        let target = marker.get(GROUP_MEMBERS_MARKER).and_then(Value::as_i64);
        let int_only = |v: Option<&Value>| v.is_some_and(|v| v.is_i64() || v.is_u64());
        if marker.len() != 2
            || !int_only(marker.get("count"))
            || count.is_none_or(|c| c < 0)
            || !int_only(marker.get(GROUP_MEMBERS_MARKER))
            || target != Some(ordinal)
        {
            return Err(fail("audit.v2 group member marker is invalid"));
        }
        let (actual, low, high): (i64, Option<i64>, Option<i64>) = self.con.query_row(
            "SELECT COUNT(*),MIN(ordinal),MAX(ordinal) FROM group_members WHERE group_ordinal=?",
            [ordinal],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if Some(actual) != count || (actual != 0 && (low, high) != (Some(0), Some(actual - 1))) {
            return Err(fail("audit.v2 group members are incomplete or unordered"));
        }
        Ok(())
    }

    /// Row count of a collection.
    pub fn count(&self, pointer: &str) -> Result<i64, StoreError> {
        self.check_read_budget()?;
        self.collections
            .get(pointer)
            .copied()
            .ok_or_else(|| fail(format!("audit.v2 collection does not exist: {pointer}")))
    }

    /// Every row of a collection in ordinal order. For `/groups` rows the `urls` marker stays in
    /// place; open it with [`AuditV2Reader::group_members`].
    pub fn iter_collection(&self, pointer: &str) -> Result<Vec<Value>, StoreError> {
        if !self.collections.contains_key(pointer) {
            return Err(fail(format!(
                "audit.v2 collection does not exist: {pointer}"
            )));
        }
        let mut stmt = self
            .con
            .prepare("SELECT value_json FROM items WHERE pointer=? ORDER BY ordinal")?;
        let mut rows = stmt.query([pointer])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            self.check_read_budget()?;
            out.push(self.decode_item(pointer, &r.get::<_, String>(0)?)?);
        }
        Ok(out)
    }

    /// Parse one stored row; the group marker stays and [`AuditV2Reader::group_members`] provides the sequence.
    pub fn decode_item(&self, _pointer: &str, raw: &str) -> Result<Value, StoreError> {
        Ok(serde_json::from_str(raw)?)
    }

    /// The lazy member sequence of a decoded `/groups` row, when its `urls` is a marker.
    pub fn group_members(&self, row: &Value) -> Option<AuditGroupMembers<'_>> {
        if self.storage_version != 3 {
            return None;
        }
        let marker = row.get("urls")?.as_object()?;
        Some(AuditGroupMembers::new(
            self,
            marker.get(GROUP_MEMBERS_MARKER)?.as_i64()?,
            marker.get("count")?.as_i64()?,
        ))
    }

    /// Stream exact ordered members, including old storage revision 2 groups.
    pub fn iter_group_members(
        &self,
        group_ordinal: i64,
        offset: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Value>, StoreError> {
        let mut out = Vec::new();
        self.for_each_group_member(group_ordinal, offset, limit, &mut |member| {
            out.push(member);
            Ok(true)
        })?;
        Ok(out)
    }

    /// Stream group members one at a time; `f` returns `false` to stop early, so a
    /// bounded page never holds more members than it accepts.
    fn for_each_group_member(
        &self,
        group_ordinal: i64,
        offset: i64,
        limit: Option<i64>,
        f: &mut dyn FnMut(Value) -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        if offset < 0 || limit.is_some_and(|l| l < 1) {
            return Err(fail("invalid group member range"));
        }
        let group = self.get_item("/groups", group_ordinal)?;
        let urls = group.get("urls");
        if urls.is_some_and(is_group_marker) && self.storage_version == 3 {
            let mut stmt = self.con.prepare(
                "SELECT value_json FROM group_members WHERE group_ordinal=? AND ordinal>=? ORDER BY ordinal LIMIT ?",
            )?;
            let mut rows = stmt.query(params![group_ordinal, offset, limit.unwrap_or(-1)])?;
            while let Some(r) = rows.next()? {
                self.check_read_budget()?;
                if !f(serde_json::from_str(&r.get::<_, String>(0)?)?)? {
                    break;
                }
            }
            Ok(())
        } else if let Some(Value::Array(items)) = urls {
            let stop = limit.map_or(items.len(), |l| items.len().min((offset + l) as usize));
            for item in items.iter().take(stop).skip(offset as usize) {
                self.check_read_budget()?;
                if !f(item.clone())? {
                    break;
                }
            }
            Ok(())
        } else {
            Err(fail("audit.v2 group has no member array"))
        }
    }

    /// Return an explicit count- and byte-bounded member page.
    pub fn group_members_page(
        &self,
        group_ordinal: i64,
        offset: i64,
        limit: i64,
        max_bytes: i64,
    ) -> Result<Map<String, Value>, StoreError> {
        if !(1..=1000).contains(&limit) || !(1..=MAX_ITEM_BYTES).contains(&max_bytes) {
            return Err(fail("group member page exceeds its count or byte bound"));
        }
        let group = self.get_item("/groups", group_ordinal)?;
        let total = match group.get("urls") {
            Some(m) if is_group_marker(m) && self.storage_version == 3 => {
                m.get("count").and_then(Value::as_i64).unwrap_or(0)
            }
            Some(Value::Array(items)) => items.len() as i64,
            _ => return Err(fail("audit.v2 group has no member array")),
        };
        let (mut output, mut used) = (Vec::new(), 0_i64);
        let mut oversized_first = false;
        self.for_each_group_member(group_ordinal, offset, Some(limit), &mut |member| {
            let size = canonical(&member)?.len() as i64;
            if used + size > max_bytes {
                oversized_first = output.is_empty();
                return Ok(false);
            }
            output.push(member);
            used += size;
            Ok(true)
        })?;
        if oversized_first {
            return Err(fail(
                "one group member exceeds the requested page byte bound",
            ));
        }
        let returned = output.len() as i64;
        let mut page = Map::new();
        page.insert("rows".into(), Value::Array(output));
        page.insert("total".into(), Value::from(total));
        page.insert("offset".into(), Value::from(offset));
        page.insert("returned".into(), Value::from(returned));
        page.insert("has_more".into(), Value::from(offset + returned < total));
        page.insert("bytes".into(), Value::from(used));
        Ok(page)
    }

    /// Read one bounded row by its exact collection key in the validated snapshot.
    pub fn get_item(&self, pointer: &str, ordinal: i64) -> Result<Value, StoreError> {
        let Some(&count) = self.collections.get(pointer) else {
            return Err(fail(format!(
                "audit.v2 collection does not exist: {pointer}"
            )));
        };
        if !(0..count).contains(&ordinal) {
            return Err(fail(format!(
                "audit.v2 collection ordinal is out of range: {ordinal}"
            )));
        }
        let raw: Option<String> = self
            .con
            .query_row(
                "SELECT value_json FROM items WHERE pointer=? AND ordinal=?",
                params![pointer, ordinal],
                |r| r.get(0),
            )
            .optional()?;
        match raw {
            Some(raw) => self.decode_item(pointer, &raw),
            None => Err(fail(format!(
                "audit.v2 collection row is missing: {pointer}[{ordinal}]"
            ))),
        }
    }

    /// Compact, complete JSON document in chunks; optionally enforce a byte ceiling.
    pub fn document_chunks(&self, max_bytes: Option<i64>) -> Result<Vec<String>, StoreError> {
        let mut out = Emitter {
            chunks: Vec::new(),
            emitted: 0,
            max_bytes,
        };
        self.walk(&self.header, true, &mut out)?;
        Ok(out.chunks)
    }

    fn walk(
        &self,
        value: &Value,
        header_markers: bool,
        out: &mut Emitter,
    ) -> Result<(), StoreError> {
        match value {
            Value::Object(map)
                if header_markers
                    && map.len() == 2
                    && map.contains_key(COLLECTION_MARKER)
                    && map.contains_key("count") =>
            {
                let pointer = map[COLLECTION_MARKER].as_str().unwrap_or("");
                if self.collections.get(pointer).copied() != map["count"].as_i64() {
                    return Err(fail(
                        "audit.v2 document marker does not match collection index",
                    ));
                }
                out.emit("[")?;
                for (index, item) in self.iter_collection(pointer)?.iter().enumerate() {
                    if index > 0 {
                        out.emit(",")?;
                    }
                    if pointer == "/groups" {
                        self.walk_group(item, out)?;
                    } else {
                        out.emit(&dumps(item, false))?;
                    }
                }
                out.emit("]")
            }
            Value::Object(map) => {
                out.emit("{")?;
                for (index, (key, child)) in map.iter().enumerate() {
                    if index > 0 {
                        out.emit(",")?;
                    }
                    out.emit(&dumps(&Value::from(key.clone()), false))?;
                    out.emit(":")?;
                    self.walk(child, header_markers, out)?;
                }
                out.emit("}")
            }
            Value::Array(items) => {
                out.emit("[")?;
                for (index, child) in items.iter().enumerate() {
                    if index > 0 {
                        out.emit(",")?;
                    }
                    self.walk(child, header_markers, out)?;
                }
                out.emit("]")
            }
            other => out.emit(&dumps(other, false)),
        }
    }

    /// A `/groups` row: its `urls` marker expands to the member array, the rest is plain.
    fn walk_group(&self, item: &Value, out: &mut Emitter) -> Result<(), StoreError> {
        let Value::Object(map) = item else {
            return self.walk(item, false, out);
        };
        out.emit("{")?;
        for (index, (key, child)) in map.iter().enumerate() {
            if index > 0 {
                out.emit(",")?;
            }
            out.emit(&dumps(&Value::from(key.clone()), false))?;
            out.emit(":")?;
            match (key.as_str(), self.group_members(item)) {
                ("urls", Some(members)) => {
                    out.emit("[")?;
                    for (i, member) in members.to_vec()?.iter().enumerate() {
                        if i > 0 {
                            out.emit(",")?;
                        }
                        out.emit(&dumps(member, false))?;
                    }
                    out.emit("]")?;
                }
                _ => self.walk(child, false, out)?,
            }
        }
        out.emit("}")
    }

    /// Explicitly materialize the compatibility document under a byte ceiling.
    pub fn materialize_legacy(&self, max_bytes: i64) -> Result<Value, StoreError> {
        Ok(serde_json::from_str(
            &self.document_chunks(Some(max_bytes))?.concat(),
        )?)
    }

    /// Release the SQLite connection (dropping the reader does the same).
    pub fn close(self) {
        drop(self);
    }
}

struct Emitter {
    chunks: Vec<String>,
    emitted: i64,
    max_bytes: Option<i64>,
}

impl Emitter {
    fn emit(&mut self, chunk: &str) -> Result<(), StoreError> {
        self.emitted += chunk.len() as i64;
        if let Some(max) = self.max_bytes {
            if self.emitted > max {
                return Err(fail(format!(
                    "legacy JSON export exceeds its {max}-byte limit"
                )));
            }
        }
        self.chunks.push(chunk.to_owned());
        Ok(())
    }
}

#[cfg(unix)]
fn nlink(m: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(m)
}

#[cfg(not(unix))]
fn nlink(_: &std::fs::Metadata) -> u64 {
    1
}
