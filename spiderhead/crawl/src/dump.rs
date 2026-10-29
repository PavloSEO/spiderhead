//! Dumps every table of a scan database to `scan.tables.json`, in rowid order.
//! Stored bodies are decoded text with the `{BASE}` placeholder stripped and re-hashed.
//! Normalisation below makes the output structurally comparable across runs.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// Rows of one table as JSON objects (`SELECT * ... ORDER BY rowid`).
fn sql_rows(con: &Connection, table: &str) -> Result<Vec<Value>, String> {
    let mut statement = con
        .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
        .map_err(|e| e.to_string())?;
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let mut out: Vec<Value> = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut object = Map::new();
        for (index, column) in columns.iter().enumerate() {
            let value: Value = match row.get_ref(index).map_err(|e| e.to_string())? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => json!(n),
                rusqlite::types::ValueRef::Real(f) => json!(f),
                rusqlite::types::ValueRef::Text(text) => {
                    let text = std::str::from_utf8(text).map_err(|e| e.to_string())?;
                    if let Some(base) = column.strip_suffix("_json") {
                        let _ = base;
                        serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
                    } else {
                        Value::String(text.to_string())
                    }
                }
                rusqlite::types::ValueRef::Blob(blob) => json!({ "$blob_len": blob.len() }),
            };
            object.insert(column.clone(), value);
        }
        out.push(Value::Object(object));
    }
    Ok(out)
}

/// Decode a stored body row: `data` is zlib (codec `zlib`) or identity bytes.
fn decode_body(codec: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if codec == "zlib" {
        use std::io::Read;
        let mut out = Vec::new();
        let mut decoder = flate2::read::ZlibDecoder::new(data);
        decoder
            .read_to_end(&mut out)
            .map_err(|e| format!("body decode failed: {e}"))?;
        Ok(out)
    } else {
        Ok(data.to_vec())
    }
}

/// Build the whole `scan.tables.json` document for one scan file.
///
/// `base_origin` is the fixture server origin (`http://127.0.0.1:<port>`),
/// `work_dir` the scratch directory the scan lived in.
pub fn scan_tables(scan_path: &Path, base_origin: &str, work_dir: &str) -> Result<Value, String> {
    let con = Connection::open_with_flags(scan_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut tables: Vec<String> = Vec::new();
    {
        let mut statement = con
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = statement.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            tables.push(row.get::<_, String>(0).map_err(|e| e.to_string())?);
        }
    }

    let mut document = Map::new();
    let mut sha_map: BTreeMap<String, String> = BTreeMap::new();
    for table in &tables {
        if table == "bodies" {
            let rows = dump_bodies(&con, base_origin, &mut sha_map)?;
            document.insert(table.clone(), Value::Array(rows));
        } else {
            let rows = sql_rows(&con, table)?;
            document.insert(table.clone(), Value::Array(rows));
        }
    }
    let mut normaliser = Norm {
        base_origin: base_origin.to_string(),
        work_dir: work_dir.to_string(),
        uuids: BTreeMap::new(),
        sha_map,
    };
    let normalised = normaliser.walk(&Value::Object(document), None);
    Ok(normalised)
}

/// Byte-level replace used for the `{BASE}` strip.
fn replace_bytes(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = rest.windows(from.len()).position(|window| window == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}

fn dump_bodies(
    con: &Connection,
    base_origin: &str,
    sha_map: &mut BTreeMap<String, String>,
) -> Result<Vec<Value>, String> {
    let mut statement = con
        .prepare("SELECT sha256, codec, data FROM bodies ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    use sha2::{Digest, Sha256};
    let origin = base_origin.as_bytes();
    let placeholder = b"{BASE}";
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let original: String = row.get(0).map_err(|e| e.to_string())?;
        let codec: String = row.get(1).map_err(|e| e.to_string())?;
        let data: Vec<u8> = row.get(2).map_err(|e| e.to_string())?;
        let decoded = decode_body(&codec, &data)?;
        // {BASE} substitution happens at the byte level: the hash and the byte
        // count cover the stripped raw body, the text is its lossy UTF-8 view.
        let mut stripped = decoded;
        if !origin.is_empty() {
            stripped = replace_bytes(&stripped, origin, placeholder);
        }
        let new_sha = format!("{:x}", Sha256::digest(&stripped));
        sha_map.insert(original, new_sha.clone());
        let text = String::from_utf8_lossy(&stripped);
        out.push(json!({
            "decoded_bytes": stripped.len(),
            "sha256": new_sha,
            "text": text,
        }));
    }
    Ok(out)
}

/// Normalisation applied before the diff, so the outputs compare structurally.
struct Norm {
    base_origin: String,
    work_dir: String,
    uuids: BTreeMap<String, String>,
    sha_map: BTreeMap<String, String>,
}

const DURATION_KEYS: &[&str] = &[
    "elapsed_ms",
    "ttfb_ms",
    "response_time",
    "elapsed_seconds",
    "duration",
    "duration_ms",
    "latency_ms",
    "total_ms",
    "time_ms",
    "crawl_delay_applied_at",
    "delay_seconds",
];

fn is_iso_datetime(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return false;
    }
    text[0..4].chars().all(|c| c.is_ascii_digit())
        && text[5..7].chars().all(|c| c.is_ascii_digit())
        && text[8..10].chars().all(|c| c.is_ascii_digit())
        && text[11..13].chars().all(|c| c.is_ascii_digit())
        && bytes[13] == b':'
        && text[14..16].chars().all(|c| c.is_ascii_digit())
        && bytes[16] == b':'
        && text[17..19].chars().all(|c| c.is_ascii_digit())
}

fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())
}

impl Norm {
    fn walk(&mut self, value: &Value, _key: Option<&str>) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                for (name, item) in map {
                    let replaced = if DURATION_KEYS.contains(&name.as_str()) && item.is_number() {
                        json!("<DURATION>")
                    } else if name == "config_fingerprint" && item.is_string() {
                        json!("<FINGERPRINT>")
                    } else if name == "digest" && item.is_string() {
                        json!("<DIGEST>")
                    } else if name == "input" && item.is_string() {
                        json!("<INPUT>")
                    } else {
                        self.walk(item, Some(name))
                    };
                    out.insert(name.clone(), replaced);
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.walk(item, _key)).collect())
            }
            Value::String(text) => Value::String(self.string(text)),
            other => other.clone(),
        }
    }

    fn string(&mut self, text: &str) -> String {
        if text == self.base_origin {
            return "{BASE}".to_string();
        }
        if is_iso_datetime(text) {
            return "<TS>".to_string();
        }
        if is_uuid(text) {
            return self.uuid_placeholder(text);
        }
        let mut replaced = text.replace(&self.base_origin, "{BASE}");
        replaced = replace_other_ports(&replaced);
        if !self.work_dir.is_empty() && replaced.contains(&self.work_dir) {
            replaced = replaced.replace(&self.work_dir, "<WORK>");
        }
        // Body-sha re-labelling: any 64-hex that was an original body sha
        // becomes the re-hashed value.
        if is_sha256_hex(&replaced) {
            if let Some(new) = self.sha_map.get(&replaced) {
                return new.clone();
            }
        }
        replaced
    }

    fn uuid_placeholder(&mut self, uuid: &str) -> String {
        let next = self.uuids.len() + 1;
        let placeholder = self
            .uuids
            .entry(uuid.to_string())
            .or_insert_with(|| format!("<UUID:{next}>"))
            .clone();
        placeholder
    }
}

fn replace_other_ports(text: &str) -> String {
    // Any `127.0.0.1:<digits>` that is not the fixture origin becomes
    // `127.0.0.1:{PORT}`. The origin itself was already replaced.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("127.0.0.1:") {
        let after = &rest[at + "127.0.0.1:".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let colon_next = after[digits.len()..].starts_with(':');
        if !digits.is_empty() && !colon_next {
            out.push_str(&rest[..at]);
            out.push_str("127.0.0.1:{PORT}");
            rest = &after[digits.len()..];
        } else {
            out.push_str(&rest[..at + "127.0.0.1:".len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Serializes JSON with one-space indent, sorted keys and unescaped non-ASCII text.
pub fn py_json_dump(value: &Value) -> String {
    let mut out = String::new();
    write_py(&sorted(value), 0, &mut out);
    out
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

fn write_py(value: &Value, depth: usize, out: &mut String) {
    let pad = " ".repeat(depth);
    let inner = " ".repeat(depth + 1);
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (index, key) in keys.iter().enumerate() {
                out.push_str(&inner);
                out.push_str(&py_string(key));
                out.push_str(": ");
                write_py(&map[*key], depth + 1, out);
                if index + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        Value::Object(_) => out.push_str("{}"),
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&inner);
                write_py(item, depth + 1, out);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        Value::Array(_) => out.push_str("[]"),
        Value::String(text) => out.push_str(&py_string(text)),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
        Value::Number(number) => out.push_str(&number.to_string()),
    }
}

/// JSON string escaping that leaves non-ASCII characters unescaped.
fn py_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_origin_ports_and_uuids() {
        let mut norm = Norm {
            base_origin: "http://127.0.0.1:18701".into(),
            work_dir: "/tmp/work".into(),
            uuids: BTreeMap::new(),
            sha_map: BTreeMap::new(),
        };
        let doc = json!({
            "scan": [{"start_url": "http://127.0.0.1:18701/",
                      "other": "http://127.0.0.1:54321/x",
                      "uuid": "0b6d8a6e-6f0e-4a2a-9a1e-2f6b7c8d9e0f",
                      "at": "2026-10-27T10:00:00.123456Z"}]
        });
        let out = norm.walk(&doc, None);
        assert_eq!(out["scan"][0]["start_url"], json!("{BASE}/"));
        assert_eq!(out["scan"][0]["other"], json!("http://127.0.0.1:{PORT}/x"));
        assert_eq!(out["scan"][0]["uuid"], json!("<UUID:1>"));
        assert_eq!(out["scan"][0]["at"], json!("<TS>"));
    }

    #[test]
    fn duration_keys_and_hashes() {
        let mut norm = Norm {
            base_origin: "http://127.0.0.1:1".into(),
            work_dir: String::new(),
            uuids: BTreeMap::new(),
            sha_map: BTreeMap::new(),
        };
        let doc = json!({
            "resume_state": [{"elapsed_seconds": 1.5, "max_depth_reached": 1}],
            "x": "f43eca2644875f130633498eaef97347753e0a7568da6d16b15a76d2e95795a9"
        });
        let out = norm.walk(&doc, None);
        assert_eq!(
            out["resume_state"][0]["elapsed_seconds"],
            json!("<DURATION>")
        );
        assert_eq!(out["resume_state"][0]["max_depth_reached"], json!(1));
        // A 64-hex that is not a body sha stays untouched.
        assert_eq!(
            out["x"],
            json!("f43eca2644875f130633498eaef97347753e0a7568da6d16b15a76d2e95795a9")
        );
    }

    #[test]
    fn py_dump_matches_reference_layout() {
        let value = json!({"b": [1, {}], "a": "x\"y"});
        let dumped = py_json_dump(&value);
        assert_eq!(
            dumped,
            "{\n \"a\": \"x\\\"y\",\n \"b\": [\n  1,\n  {}\n ]\n}"
        );
    }
}
