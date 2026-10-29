//! Row-level SQL helpers, JSONL streaming and URL interning.

use super::*;

/// Map an SQL value to JSON; BLOB columns never occur in scan.v1.
pub(super) fn sql_cell(value: ValueRef<'_>) -> Result<Value, StoreError> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::String(
            std::str::from_utf8(t)
                .map_err(|e| StoreError::Invalid(format!("utf-8: {e}")))?
                .to_owned(),
        ),
        ValueRef::Blob(_) => return Err(bad("unexpected BLOB value in scan.v1")),
    })
}

/// Every row of `sql` as a map from column name to value.
pub(crate) fn query_maps(
    con: &Connection,
    sql: &str,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let mut out = Vec::new();
    for_each_map(con, sql, &mut |map| {
        out.push(map);
        Ok(true)
    })?;
    Ok(out)
}

/// Stream rows of `sql` as column-name maps, one at a time;
/// `each` returns `false` to stop the walk early.
pub(crate) fn for_each_map(
    con: &Connection,
    sql: &str,
    each: &mut dyn FnMut(Map<String, Value>) -> Result<bool, StoreError>,
) -> Result<(), StoreError> {
    let mut st = con.prepare(sql)?;
    let names: Vec<String> = st.column_names().iter().map(|n| (*n).to_owned()).collect();
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let mut map = Map::new();
        for (i, name) in names.iter().enumerate() {
            map.insert(name.clone(), sql_cell(row.get_ref(i)?)?);
        }
        if !each(map)? {
            break;
        }
    }
    Ok(())
}

/// Text field of a row map.
pub(crate) fn field_str<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str, StoreError> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("missing or non-text field: {key}")))
}

/// Integer field of a row map.
pub(crate) fn field_int(map: &Map<String, Value>, key: &str) -> Result<i64, StoreError> {
    map.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| bad(format!("missing or non-integer field: {key}")))
}

/// Insert one row, checking field names and SQLite storage classes against the expected schema.
pub fn insert(con: &Connection, table: &str, row: &Map<String, Value>) -> Result<(), StoreError> {
    let columns = columns_of(table)?;
    let known: BTreeSet<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    let unknown: Vec<&str> = row
        .keys()
        .map(String::as_str)
        .filter(|k| !known.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(bad(format!("{table}: unknown fields {unknown:?}")));
    }
    for column in &columns {
        let value = row.get(&column.name).unwrap_or(&Value::Null);
        if value.is_null() {
            if column.required && column.default.is_none() {
                return Err(bad(format!(
                    "{table}.{}: required value is missing",
                    column.name
                )));
            }
            continue;
        }
        let valid = match column.kind.as_str() {
            "INTEGER" => value.as_i64().is_some(),
            "REAL" => value.is_number(),
            "TEXT" => value.is_string(),
            // JSON never yields bytes, so a BLOB column always refuses a JSON value.
            _ => false,
        };
        if !valid {
            return Err(bad(format!(
                "{table}.{}: expected {}",
                column.name, column.kind
            )));
        }
    }
    let mut names = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for (name, value) in row {
        names.push(format!("\"{name}\""));
        params.push(match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
            Value::Number(n) => match n.as_i64() {
                Some(i) => rusqlite::types::Value::Integer(i),
                None => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
            },
            Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            _ => return Err(bad(format!("{table}.{name}: expected a scalar value"))),
        });
    }
    let placeholders = vec!["?"; params.len()].join(",");
    con.execute(
        &format!(
            "INSERT INTO \"{table}\" ({}) VALUES ({placeholders})",
            names.join(",")
        ),
        rusqlite::params_from_iter(params),
    )?;
    Ok(())
}

/// Stream a legacy JSONL file record by record (strict JSON per line).
///
/// A truncated final line is recovered once and recorded in `limitations`.
/// The file's size and mtime must not change while it is read. The `inputs`
/// entry (name, sha256, bytes) is appended when the stream ends cleanly.
/// Nothing is buffered beyond the current line.
pub fn jsonl<'a>(
    path: &Path,
    limitations: &'a mut Vec<String>,
    inputs: &'a mut Vec<Map<String, Value>>,
) -> Result<JsonlRows<'a>, StoreError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let reader = BufReader::new(File::open(path)?);
    let initial = reader.get_ref().metadata()?;
    Ok(JsonlRows {
        name,
        reader,
        initial,
        digest: Sha256::new(),
        size: 0,
        number: 0,
        done: false,
        limitations,
        inputs,
    })
}

/// Iterator returned by [`jsonl`]; yields one JSON object per line.
pub struct JsonlRows<'a> {
    name: String,
    reader: BufReader<File>,
    initial: fs::Metadata,
    digest: Sha256,
    size: u64,
    number: u64,
    done: bool,
    limitations: &'a mut Vec<String>,
    inputs: &'a mut Vec<Map<String, Value>>,
}

impl JsonlRows<'_> {
    /// End-of-file checks: the source must be unchanged, then the input is recorded.
    fn finish(&mut self) -> Result<(), StoreError> {
        let final_meta = self.reader.get_ref().metadata()?;
        if (
            self.initial.size(),
            self.initial.mtime(),
            self.initial.mtime_nsec(),
        ) != (
            final_meta.size(),
            final_meta.mtime(),
            final_meta.mtime_nsec(),
        ) {
            return Err(bad(format!(
                "{}: source changed while importing; retry a finished run",
                self.name
            )));
        }
        let mut input = Map::new();
        input.insert("name".into(), Value::String(self.name.clone()));
        input.insert(
            "sha256".into(),
            Value::String(format!("{:x}", self.digest.clone().finalize())),
        );
        input.insert("bytes".into(), Value::from(self.size));
        self.inputs.push(input);
        Ok(())
    }

    fn next_row(&mut self) -> Result<Option<Map<String, Value>>, StoreError> {
        let limit = MAX_RECORD_BYTES as u64 + 1;
        let mut raw = Vec::new();
        let read = (&mut self.reader).take(limit).read_until(b'\n', &mut raw)?;
        if read == 0 {
            self.finish()?;
            return Ok(None);
        }
        self.number += 1;
        self.size += read as u64;
        self.digest.update(&raw);
        let (name, number) = (self.name.clone(), self.number);
        if raw.len() as i64 > MAX_RECORD_BYTES {
            return Err(bad(format!("{name}:{number}: exceeds the 8 MiB row limit")));
        }
        let label = format!("{name}:{number}");
        let parsed = match std::str::from_utf8(&raw) {
            Ok(text) => parse_json(text)
                .map_err(|e| (e.is_eof(), bad(format!("{label}: invalid JSON ({e})")))),
            Err(e) => Err((
                e.error_len().is_none(),
                StoreError::Invalid(format!("{label}: utf-8: {e}")),
            )),
        };
        match parsed {
            Ok(Value::Object(map)) => Ok(Some(map)),
            Ok(_) => Err(bad(format!("{label}: expected a JSON object"))),
            Err((truncated, err)) => {
                let mut probe = [0u8; 1];
                let at_end = self.reader.read(&mut probe)? == 0;
                if truncated && !raw.ends_with(b"\n") && at_end {
                    if recovered(self.limitations) {
                        return Err(bad(
                            "only one truncated final JSONL record may be recovered",
                        ));
                    }
                    self.limitations
                        .push(format!("{name}: recovered a truncated final line {number}"));
                    self.finish()?;
                    return Ok(None);
                }
                Err(err)
            }
        }
    }
}

impl Iterator for JsonlRows<'_> {
    type Item = Result<Map<String, Value>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_row() {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

/// Intern a URL in the `urls` table and return its id.
pub fn url(con: &Connection, url: &str) -> Result<i64, StoreError> {
    if url.is_empty() {
        return Err(bad("a page/link URL must be a nonempty string"));
    }
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES (?)", [url])?;
    Ok(con.query_row("SELECT url_id FROM urls WHERE url = ?", [url], |r| r.get(0))?)
}
