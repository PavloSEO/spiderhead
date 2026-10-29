//! `sources_db.status`/`query` — read-only coverage summary and bounded export.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde_json::{json, Map, Value};

use super::{
    day_list, gaps, invalid, py_path_str, source_spec, SourcesError, MAX_EXPORT_ROWS,
    SCHEMA_VERSION,
};
use crate::{pyjson, pystr, pytime};

/// `status` — summarize requested and retained calendar-day coverage without modifying
/// the database.
pub fn status(path: &Path) -> Result<Value, SourcesError> {
    let connection = super::connect(path, false)?;
    connection.execute_batch("BEGIN")?;
    // Ledger keyed by (source, resource); `ORDER BY source, resource, date` matches the
    // BTreeMap byte-order iteration.
    type DayRow = (String, i64, String, Option<String>, Option<String>);
    let mut ledger: BTreeMap<(String, String), Vec<DayRow>> = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT source, resource, date, rows, state, fetched_at, last_attempt_state \
             FROM sync_log ORDER BY source, resource, date",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (source, resource, day, count, state, fetched_at, attempt) = row?;
            ledger
                .entry((source, resource))
                .or_default()
                .push((day, count, state, fetched_at, attempt));
        }
    }
    let mut entries = Vec::new();
    for ((source, resource), days) in &ledger {
        // A ledger row for an unregistered source is a key error.
        let spec = source_spec(source).map_err(|_| SourcesError::Key(pystr::repr_str(source)))?;
        let stored: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {} WHERE resource = ?", spec.table),
            rusqlite::params![resource],
            |row| row.get(0),
        )?;
        let dates: Vec<&str> = days.iter().map(|(day, ..)| day.as_str()).collect();
        let mut coverage = Map::new();
        for state in [
            "complete", "empty", "partial", "failed", "pending", "lagged",
        ] {
            coverage.insert(
                state.to_owned(),
                json!(days
                    .iter()
                    .filter(|(.., current, _, _)| current == state)
                    .map(|(day, ..)| json!(day))
                    .collect::<Vec<_>>()),
            );
        }
        let requested: std::collections::BTreeSet<&str> = dates.iter().copied().collect();
        coverage.insert(
            "unrequested".to_owned(),
            json!(day_list(dates[0], dates[dates.len() - 1])?
                .into_iter()
                .filter(|day| !requested.contains(day.as_str()))
                .collect::<Vec<_>>()),
        );
        let mut entry = Map::new();
        entry.insert("source".to_owned(), json!(source));
        entry.insert("resource".to_owned(), json!(resource));
        entry.insert("schema_version".to_owned(), json!(SCHEMA_VERSION));
        entry.insert(
            "dimensions".to_owned(),
            json!(spec.dimensions.iter().collect::<Vec<_>>()),
        );
        entry.insert(
            "metrics".to_owned(),
            json!(spec.metrics.iter().collect::<Vec<_>>()),
        );
        entry.insert(
            "non_additive_metrics".to_owned(),
            json!(spec.non_additive.iter().collect::<Vec<_>>()),
        );
        entry.insert("date_timezone".to_owned(), json!(spec.date_timezone));
        entry.insert(
            "lag_cutoff_policy".to_owned(),
            json!(if spec.date_timezone == "America/Los_Angeles" {
                "resource_pacific_date"
            } else {
                "conservative_utc_minus_one_day"
            }),
        );
        entry.insert("rows".to_owned(), json!(stored));
        entry.insert("first_date".to_owned(), json!(dates[0]));
        entry.insert("last_date".to_owned(), json!(dates[dates.len() - 1]));
        entry.insert("requested_days".to_owned(), json!(days.len() as i64));
        let complete = coverage["complete"].as_array().map(Vec::len).unwrap_or(0);
        let empty = coverage["empty"].as_array().map(Vec::len).unwrap_or(0);
        entry.insert("synced_days".to_owned(), json!((complete + empty) as i64));
        entry.insert("empty_days".to_owned(), json!(empty as i64));
        entry.insert("incomplete_days".to_owned(), coverage["partial"].clone());
        entry.insert("coverage".to_owned(), Value::Object(coverage));
        entry.insert(
            "gaps".to_owned(),
            json!(gaps(
                &dates.iter().map(|d| d.to_string()).collect::<Vec<_>>()
            )?),
        );
        entry.insert(
            "last_fetched_at".to_owned(),
            days.iter()
                .filter_map(|(.., fetched, _)| fetched.as_ref().filter(|s| !s.is_empty()))
                .max()
                .cloned()
                .map(Value::String)
                .unwrap_or_default(),
        );
        let mut attempts = Map::new();
        for (day, .., attempt) in days {
            if let Some(attempt) = attempt
                .as_ref()
                .filter(|s| !s.is_empty() && s.as_str() != "complete" && s.as_str() != "empty")
            {
                attempts.insert(day.clone(), json!(attempt));
            }
        }
        entry.insert("last_attempts".to_owned(), Value::Object(attempts));
        entries.push(Value::Object(entry));
    }
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(true));
    out.insert("db".to_owned(), json!(py_path_str(path)));
    out.insert("schema_version".to_owned(), json!(SCHEMA_VERSION));
    out.insert("sources".to_owned(), json!(entries));
    Ok(Value::Object(out))
}

/// `query`'s keyword arguments.
pub struct QueryArgs<'a> {
    /// Restrict to one resource (falsy = all).
    pub resource: Option<&'a str>,
    /// Inclusive ISO date bounds (falsy = unbounded, each validated when truthy).
    pub start_date: Option<&'a str>,
    /// Inclusive end date.
    pub end_date: Option<&'a str>,
    /// Substring matched against every dimension (`instr`).
    pub match_text: Option<&'a str>,
    /// Page cap, 1..=`MAX_EXPORT_ROWS` (default 1000).
    pub limit: i64,
    /// CSV target — never overwritten, mode 0600.
    pub out: Option<&'a str>,
}

/// Repr-style text of a byte string: `\xNN` escapes, `\t\n\r` shorthands, and `'` quoting.
fn repr_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push_str("b'");
    for &byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push('\'');
    out
}

/// One CSV cell: text of the value, `None` as an empty cell, and quoting for fields containing
/// `,`, `"`, `\r` or `\n`. The line terminator is `\r\n`.
fn csv_field(value: rusqlite::types::ValueRef<'_>) -> String {
    let text = match value {
        rusqlite::types::ValueRef::Null => String::new(),
        rusqlite::types::ValueRef::Integer(n) => n.to_string(),
        rusqlite::types::ValueRef::Real(x) => pyjson::float_repr(x),
        rusqlite::types::ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        rusqlite::types::ValueRef::Blob(bytes) => repr_bytes(bytes),
    };
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

/// `query` — read bounded ordered rows with the coverage state of each retained day.
pub fn query(path: &Path, source: &str, args: &QueryArgs<'_>) -> Result<Value, SourcesError> {
    let spec = source_spec(source)?;
    if !(1..=MAX_EXPORT_ROWS).contains(&args.limit) {
        return invalid(format!("limit must be between 1 and {MAX_EXPORT_ROWS}"));
    }
    for value in [args.start_date, args.end_date].into_iter().flatten() {
        if !value.is_empty() {
            if let Err(message) = pytime::date_fromisoformat_err(value) {
                return invalid(message);
            }
        }
    }
    if let (Some(start), Some(end)) = (args.start_date, args.end_date) {
        if !start.is_empty() && !end.is_empty() && start > end {
            return invalid("start_date must not be after end_date");
        }
    }
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> =
        vec![rusqlite::types::Value::Text(source.to_owned())];
    if let Some(resource) = args.resource.filter(|r| !r.is_empty()) {
        clauses.push("facts.resource = ?".to_owned());
        params.push(rusqlite::types::Value::Text(resource.to_owned()));
    }
    if let Some(start) = args.start_date.filter(|d| !d.is_empty()) {
        clauses.push("facts.date >= ?".to_owned());
        params.push(rusqlite::types::Value::Text(start.to_owned()));
    }
    if let Some(end) = args.end_date.filter(|d| !d.is_empty()) {
        clauses.push("facts.date <= ?".to_owned());
        params.push(rusqlite::types::Value::Text(end.to_owned()));
    }
    if let Some(text) = args.match_text.filter(|t| !t.is_empty()) {
        let parts: Vec<String> = spec
            .dimensions
            .iter()
            .map(|d| format!("instr(facts.{d}, ?) > 0"))
            .collect();
        clauses.push(format!("({})", parts.join(" OR ")));
        for _ in spec.dimensions {
            params.push(rusqlite::types::Value::Text(text.to_owned()));
        }
    }
    let mut fact_columns = vec!["resource", "date"];
    fact_columns.extend(spec.dimensions.iter());
    fact_columns.extend(spec.metrics.iter());
    let mut columns: Vec<String> = fact_columns.iter().map(|c| c.to_string()).collect();
    columns.push("coverage_state".to_owned());
    let mut sql = format!(
        "SELECT {} FROM {} AS facts \
         JOIN sync_log ON sync_log.source=? AND sync_log.resource=facts.resource \
         AND sync_log.date=facts.date",
        fact_columns
            .iter()
            .map(|column| format!("facts.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
            + ", sync_log.state",
        spec.table
    );
    if !clauses.is_empty() {
        sql += " WHERE ";
        sql += &clauses.join(" AND ");
    }
    sql += " ORDER BY facts.resource, facts.date, ";
    sql += &spec
        .dimensions
        .iter()
        .map(|name| format!("facts.{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let connection = super::connect(path, false)?;
    params.push(rusqlite::types::Value::Integer(args.limit + 1));
    let sql = format!("{sql} LIMIT ?");
    if let Some(out) = args.out {
        let target = Path::new(out);
        if target.exists() {
            return invalid(format!(
                "refusing to overwrite existing file {}",
                py_path_str(target)
            ));
        }
        let mut count = 0i64;
        let mut has_more = false;
        let mut created = false;
        let write_result = (|| -> Result<(), SourcesError> {
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut handle = options.open(target)?;
            created = true;
            handle.write_all(
                columns
                    .iter()
                    .map(|c| csv_field(rusqlite::types::ValueRef::Text(c.as_bytes())))
                    .collect::<Vec<_>>()
                    .join(",")
                    .as_bytes(),
            )?;
            handle.write_all(b"\r\n")?;
            let mut statement = connection.prepare(&sql)?;
            let mut cursor = statement.query(rusqlite::params_from_iter(params.iter()))?;
            while let Some(row) = cursor.next()? {
                if count == args.limit {
                    has_more = true;
                    break;
                }
                let mut fields = Vec::with_capacity(columns.len());
                for index in 0..columns.len() {
                    fields.push(csv_field(row.get_ref(index)?));
                }
                handle.write_all(fields.join(",").as_bytes())?;
                handle.write_all(b"\r\n")?;
                count += 1;
            }
            Ok(())
        })();
        if let Err(err) = write_result {
            if created {
                let _ = std::fs::remove_file(target);
            }
            return Err(err);
        }
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(true));
        result.insert("source".to_owned(), json!(source));
        result.insert("out".to_owned(), json!(py_path_str(target)));
        result.insert("rows".to_owned(), json!(count));
        result.insert("has_more".to_owned(), json!(has_more));
        result.insert(
            "non_additive_metrics".to_owned(),
            json!(spec.non_additive.iter().collect::<Vec<_>>()),
        );
        return Ok(Value::Object(result));
    }
    let mut rows = Vec::new();
    {
        let mut statement = connection.prepare(&sql)?;
        let mut cursor = statement.query(rusqlite::params_from_iter(params.iter()))?;
        while let Some(row) = cursor.next()? {
            let mut entry = Map::new();
            for (index, column) in columns.iter().enumerate() {
                entry.insert(column.clone(), sqlite_json(row.get_ref(index)?)?);
            }
            rows.push(Value::Object(entry));
        }
    }
    let returned = rows.len().min(args.limit as usize) as i64;
    let has_more = rows.len() > args.limit as usize;
    rows.truncate(args.limit as usize);
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(true));
    out.insert("source".to_owned(), json!(source));
    out.insert(
        "columns".to_owned(),
        json!(columns.iter().collect::<Vec<_>>()),
    );
    out.insert("rows".to_owned(), json!(rows));
    out.insert("returned".to_owned(), json!(returned));
    out.insert("has_more".to_owned(), json!(has_more));
    out.insert(
        "non_additive_metrics".to_owned(),
        json!(spec.non_additive.iter().collect::<Vec<_>>()),
    );
    out.insert("date_timezone".to_owned(), json!(spec.date_timezone));
    out.insert(
        "lag_cutoff_policy".to_owned(),
        json!(if spec.date_timezone == "America/Los_Angeles" {
            "resource_pacific_date"
        } else {
            "conservative_utc_minus_one_day"
        }),
    );
    Ok(Value::Object(out))
}

/// A SQLite value as JSON. Blobs are not representable, and the table contract never stores them.
fn sqlite_json(value: rusqlite::types::ValueRef<'_>) -> Result<Value, rusqlite::Error> {
    Ok(match value {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(n) => json!(n),
        rusqlite::types::ValueRef::Real(x) => json!(x),
        rusqlite::types::ValueRef::Text(bytes) => Value::String(
            std::str::from_utf8(bytes)
                .map_err(rusqlite::Error::Utf8Error)?
                .to_owned(),
        ),
        rusqlite::types::ValueRef::Blob(_) => {
            return Err(rusqlite::Error::InvalidColumnType(
                0,
                "blob".to_owned(),
                rusqlite::types::Type::Blob,
            ))
        }
    })
}
