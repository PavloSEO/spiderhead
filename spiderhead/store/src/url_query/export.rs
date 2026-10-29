//! Writes a validated query to one CSV or XLSX file.

use std::path::Path;

use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::run::string_list;
use super::*;
use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::url_query_filters::{where_, Params};

fn export_failure(reason_code: &str, error: String) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("ok".into(), json!(false));
    out.insert("format".into(), json!(EXPORT_FORMAT));
    out.insert("reason_code".into(), json!(reason_code));
    out.insert("error".into(), json!(error));
    out
}

/// Arguments of one query export, with the documented defaults.
#[derive(Debug, Clone)]
pub struct ExportArgs {
    /// `csv` or `xlsx`.
    pub fmt: Value,
    /// Preset view id.
    pub preset: Value,
    /// Filter objects.
    pub filters: Value,
    /// Column names.
    pub columns: Value,
    /// Row cap; the export refuses above it instead of truncating.
    pub max_rows: Value,
}

impl Default for ExportArgs {
    fn default() -> Self {
        ExportArgs {
            fmt: json!("csv"),
            preset: Value::Null,
            filters: Value::Null,
            columns: Value::Null,
            max_rows: json!(EXPORT_MAX_ROWS),
        }
    }
}

impl ExportArgs {
    /// Arguments from a JSON object with snake_case keys; unknown keys are ignored.
    pub fn from_kwargs(kwargs: &Map<String, Value>) -> Self {
        let mut args = ExportArgs::default();
        for (key, value) in kwargs {
            let slot = match key.as_str() {
                "fmt" => &mut args.fmt,
                "preset" => &mut args.preset,
                "filters" => &mut args.filters,
                "columns" => &mut args.columns,
                "max_rows" => &mut args.max_rows,
                _ => continue,
            };
            *slot = value.clone();
        }
        args
    }
}

/// Write every row a validated query matches to one new CSV or XLSX file, read-only on the scan.
///
/// Rows are read in url_id order in batches, so memory stays flat. The export refuses above
/// `max_rows` instead of truncating, and publication is all-or-nothing via scan_export's writer.
pub fn export_scan_query(input_path: &str, out: &Value, args: &ExportArgs) -> Map<String, Value> {
    match export(input_path, out, args) {
        Ok(v) => v,
        Err(StoreError::QueryError(e)) => export_failure(&e.reason_code, e.message),
        Err(StoreError::ScanError(e)) => export_failure("export_failed", e.message),
        Err(StoreError::Sqlite(e)) => {
            let reason = if is_interrupt(&e) {
                "query_timeout"
            } else {
                "cannot_read"
            };
            export_failure(reason, format!("cannot read scan: {e}"))
        }
        Err(other) => export_failure("export_failed", other.to_string()),
    }
}

/// Validate the export request, then stream the rows into the chosen file format.
pub fn export(
    input_path: &str,
    out: &Value,
    args: &ExportArgs,
) -> Result<Map<String, Value>, StoreError> {
    use crate::scan_export::{publish_files, write_csv_bytes, write_xlsx, Selection, Writer};

    let fmt = match args.fmt.as_str() {
        Some(f) if EXPORT_FORMATS.contains(&f) => f.to_string(),
        _ => {
            return Err(QueryError::new(
                "invalid_export_format",
                "export format must be csv or xlsx",
            )
            .into())
        }
    };
    let max_rows = match args.max_rows.as_i64() {
        Some(m) if (1..=EXPORT_XLSX_ROWS).contains(&m) && !args.max_rows.is_boolean() => m,
        _ => {
            return Err(QueryError::new(
                "invalid_max_rows",
                format!("max_rows must be an integer 1..{EXPORT_XLSX_ROWS}"),
            )
            .into())
        }
    };
    let Some(out_text) = out.as_str().filter(|s| !s.is_empty()) else {
        return Err(QueryError::new("invalid_output", "out must name a file path").into());
    };
    let destination = std::path::PathBuf::from(out_text);
    let parent = destination
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .unwrap_or(Path::new("."))
        .to_path_buf();
    if !parent.is_dir() {
        return Err(QueryError::new(
            "invalid_output",
            format!("output directory does not exist: {}", parent.display()),
        )
        .into());
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(QueryError::new(
            "output_exists",
            format!("output already exists: {}", destination.display()),
        )
        .into());
    }
    // The same validation as a normal read, with one row: it resolves the preset and reports the count.
    let probe_args = QueryArgs {
        filters: args.filters.clone(),
        preset: args.preset.clone(),
        columns: args.columns.clone(),
        limit: json!(1),
        ..QueryArgs::default()
    };
    let probe = scan_url_query(input_path, &probe_args);
    let probe_ok = probe.get("ok") == Some(&json!(true))
        && probe.contains_key("rows")
        && probe.get("state") != Some(&json!("unavailable"));
    if !probe_ok {
        let mut failed = probe;
        failed.insert("ok".into(), json!(false));
        failed.insert("format".into(), json!(EXPORT_FORMAT));
        return Ok(failed);
    }
    let Some(expected) = probe.get("filtered_total").and_then(Value::as_i64) else {
        return Err(QueryError::new(
            "export_count_unavailable",
            "the matching row count exceeded its time budget; narrow the filters",
        )
        .into());
    };
    if expected > max_rows {
        return Err(QueryError::new(
            "export_too_large",
            format!(
                "{expected} rows match, above max_rows {max_rows}; narrow the filters or raise max_rows (up to {EXPORT_XLSX_ROWS})"
            ),
        )
        .into());
    }
    let fields: Vec<String> = string_list(&probe["columns"]).unwrap_or_default();
    let probe_filters: Vec<Value> = probe["filters"].as_array().cloned().unwrap_or_default();
    let (conditions, params, _) = where_(&probe_filters, false, None)?;
    let select = fields
        .iter()
        .enumerate()
        .map(|(i, n)| format!("{} AS c{i}", columns()[n].expr))
        .collect::<Vec<_>>()
        .join(",");
    let body = if conditions.is_empty() {
        "p.url_id > ?".to_string()
    } else {
        format!("{conditions} AND p.url_id > ?")
    };
    let sql = format!(
        "SELECT p.url_id,{select} FROM pages p JOIN urls u ON u.url_id=p.url_id WHERE {body} ORDER BY p.url_id LIMIT ?"
    );
    let (con, source, _) = open(input_path)?;
    if fields.iter().any(|f| f == "issue_count") {
        let outcome = crate::issue_selection::materialize(
            &con,
            Path::new(input_path),
            &[],
            None,
            PAGE_BUDGET_SECONDS,
        );
        if let Err(e) = outcome {
            let text = match &e {
                StoreError::ScanError(s) => s.message.clone(),
                StoreError::Invalid(m) => m.clone(),
                other => other.to_string(),
            };
            return Err(QueryError::with_state(
                "issue_index_missing",
                format!("saved findings are unavailable: {text}"),
                "unavailable",
            )
            .into());
        }
    }
    let head = json!({
        "format": EXPORT_FORMAT,
        "preset": args.preset,
        "filters": probe["filters"],
        "columns": probe["columns"],
        "rows": expected,
        "source": source,
    });
    let written = std::cell::Cell::new(0_i64);
    let make_records = || -> crate::scan_export::RecordStream<'_> {
        Box::new(QueryRecords {
            con: &con,
            budget: Budget::new(&con),
            sql: &sql,
            params: &params,
            fields: &fields,
            expected,
            last: -1,
            buffer: std::collections::VecDeque::new(),
            done: false,
            written: &written,
        })
    };
    let mut files = indexmap::IndexMap::new();
    files.insert(fmt.clone(), destination.clone());
    let writer: Writer<'_> = if fmt == "csv" {
        Box::new(|target, owned| write_csv_bytes(target, &fields, make_records(), owned))
    } else {
        Box::new(|target, owned| {
            write_xlsx(
                target,
                &head,
                vec![Selection {
                    name: "pages".into(),
                    fields: fields.clone(),
                    stream: make_records(),
                }],
                owned,
            )
        })
    };
    let published = publish_files(&files, vec![(fmt.clone(), writer)]);
    drop(con);
    published?;
    Ok(json!({
        "ok": true,
        "format": EXPORT_FORMAT,
        "fmt": fmt,
        "file": destination.to_string_lossy(),
        "rows": written.get(),
        "preset": args.preset,
        "filters": probe["filters"],
        "columns": probe["columns"],
        "coverage": probe["coverage"],
    })
    .as_object()
    .cloned()
    .unwrap_or_default())
}

/// Batched url_id-ordered record stream behind an export.
struct QueryRecords<'a> {
    con: &'a Connection,
    budget: Budget<'a>,
    sql: &'a str,
    params: &'a Params,
    fields: &'a [String],
    expected: i64,
    last: i64,
    buffer: std::collections::VecDeque<Map<String, Value>>,
    done: bool,
    written: &'a std::cell::Cell<i64>,
}

impl QueryRecords<'_> {
    fn refill(&mut self) -> Result<(), StoreError> {
        self.budget.start(PAGE_BUDGET_SECONDS);
        let mut bound: Params = self.params.clone();
        bound.push(SqlValue::Integer(self.last));
        bound.push(SqlValue::Integer(MAX_LIMIT));
        let mut stmt = self.con.prepare(self.sql)?;
        let mut rows = stmt.query(params_from_iter(bound.iter()))?;
        let mut fetched = 0;
        while let Some(r) = rows.next()? {
            fetched += 1;
            self.last = r.get(0)?;
            let mut item = Map::new();
            for (i, name) in self.fields.iter().enumerate() {
                item.insert(name.clone(), cell_value(r.get_ref(i + 1)?));
            }
            if let Some(v) = item.get("indexable").filter(|v| !v.is_null()).cloned() {
                item.insert("indexable".into(), json!(v.as_i64().is_none_or(|n| n != 0)));
            }
            self.buffer.push_back(item);
        }
        if fetched < MAX_LIMIT {
            self.done = true;
        }
        Ok(())
    }
}

impl Iterator for QueryRecords<'_> {
    type Item = Result<Map<String, Value>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer.is_empty() && !self.done {
            if let Err(e) = self.refill() {
                self.done = true;
                return Some(Err(e));
            }
        }
        match self.buffer.pop_front() {
            Some(item) => {
                self.written.set(self.written.get() + 1);
                Some(Ok(item))
            }
            None => {
                self.budget.stop();
                if self.written.get() != self.expected && self.done {
                    self.done = true;
                    let w = self.written.get();
                    self.written.set(self.expected);
                    return Some(Err(QueryError::new(
                        "export_incomplete",
                        format!(
                            "wrote {w} rows but the count said {}; the scan changed meanwhile",
                            self.expected
                        ),
                    )
                    .into()));
                }
                None
            }
        }
    }
}
