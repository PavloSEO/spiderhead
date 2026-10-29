//! Track paid-provider usage: what was charged, for which operation, and when.
//!
//! A charge is recorded as soon as the provider returns its cost, before the result is parsed, so a
//! parser failure cannot turn a paid response into an untraceable expense. The format is JSONL, one line
//! per call; appending a line is atomic. The default path is `~/.config/seohead/spend.jsonl`, overridden
//! by `SPIDERHEAD_SPEND_LOG`/`SEOHEAD_SPEND_LOG`.

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_core::envvar::read_env;
use spiderhead_core::host::Host;
use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

tokio::task_local! {
    /// Spend attribution tags for the current task.
    static CONTEXT: Map<String, Value>
}

/// Attach `tags` to the `extra` of every entry recorded inside `f` (`spend.context`).
pub async fn context<R>(tags: Map<String, Value>, f: impl Future<Output = R>) -> R {
    let merged = CONTEXT
        .try_with(|current| {
            let mut merged = current.clone();
            merged.extend(tags.clone());
            merged
        })
        .unwrap_or_else(|_| tags.clone());
    CONTEXT.scope(merged, f).await
}

/// The spend journal path (`log_path`): `SPIDERHEAD_SPEND_LOG`/`SEOHEAD_SPEND_LOG` or
/// `~/.config/seohead/spend.jsonl`.
pub fn log_path(host: &dyn Host) -> PathBuf {
    if let Some(override_) = read_env(host, "SEOHEAD_SPEND_LOG").filter(|v| !v.is_empty()) {
        return PathBuf::from(expanduser(host, &override_));
    }
    default_home(host).join(".config/seohead/spend.jsonl")
}

fn expanduser(host: &dyn Host, value: &str) -> String {
    if let Some(rest) = value.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            return format!("{}{rest}", default_home(host).display());
        }
    }
    value.to_owned()
}

fn default_home(host: &dyn Host) -> PathBuf {
    host.var("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("~"))
}

/// Local wall-clock time as `YYYY-MM-DDTHH:MM:SS`.
#[cfg(unix)]
pub fn local_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // SAFETY: localtime_r writes into the provided tm and returns it; tm is plain data.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs as *const i64 as *const libc::time_t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// Non-Unix fallback: UTC in the same format, because local time zone lookup is Unix-only here.
#[cfg(not(unix))]
pub fn local_stamp() -> String {
    spiderhead_core::timeutil::stamp(spiderhead_core::timeutil::Format::Iso)
        .trim_end_matches("+00:00")
        .to_owned()
}

/// One ledger entry written by [`record`].
#[derive(Debug, Clone)]
pub struct Record {
    /// Provider name (`arsenkin`, `yandex_cloud`, ...).
    pub source: String,
    /// Operation inside the provider.
    pub operation: String,
    /// Amount charged in the provider's own `unit`; `None` when the cost is unknown.
    pub cost: Option<f64>,
    /// Unit label (`limits`, `requests`, ...).
    pub unit: String,
    /// Provider task identifier when the operation created one.
    pub task_id: Option<Value>,
    /// Number of items the operation covered; `None` means the caller did not pass it, and the
    /// ledger entry then stores `0`.
    pub items: Option<i64>,
    /// Extra attribution fields merged over the context tags.
    pub extra: Option<Map<String, Value>>,
    /// `at` override for deterministic tests; `None` stamps the local wall clock.
    pub at: Option<String>,
}

/// Record one charge and return the stored entry.
pub fn record(host: &dyn Host, rec: &Record) -> std::io::Result<Value> {
    let mut entry = Map::new();
    entry.insert(
        "at".to_owned(),
        Value::String(rec.at.clone().unwrap_or_else(local_stamp)),
    );
    entry.insert("source".to_owned(), json!(rec.source));
    entry.insert("operation".to_owned(), json!(rec.operation));
    entry.insert(
        "cost".to_owned(),
        rec.cost.map_or(Value::Null, |c| json!(c)),
    );
    entry.insert("unit".to_owned(), json!(rec.unit));
    entry.insert("items".to_owned(), json!(rec.items.unwrap_or(0)));
    if let Some(task_id) = &rec.task_id {
        entry.insert("task_id".to_owned(), task_id.clone());
    }
    let tags = CONTEXT.try_with(Clone::clone).ok();
    let mut extra = tags.unwrap_or_default();
    if let Some(more) = &rec.extra {
        extra.extend(more.clone());
    }
    if !extra.is_empty() {
        entry.insert("extra".to_owned(), Value::Object(extra));
    }
    let path = log_path(host);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut handle = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    handle.write_all(
        crate::pyjson::dumps_opts(&Value::Object(entry.clone()), false, false).as_bytes(),
    )?;
    handle.write_all(b"\n")?;
    Ok(Value::Object(entry))
}

/// All journal rows (`read_all`); malformed lines are skipped, uncertain zero-cost receipts
/// are normalized to `cost: null` on read only.
pub fn read_all(host: &dyn Host) -> Vec<Value> {
    let path = log_path(host);
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(mut row) => {
                if row.is_object() && is_uncertain(&row) && row.get("cost") == Some(&json!(0)) {
                    if let Some(map) = row.as_object_mut() {
                        map.insert("cost".to_owned(), Value::Null);
                    }
                }
                rows.push(row);
            }
            Err(_) => continue,
        }
    }
    rows
}

/// A receipt whose cost or charge status is not actually known.
pub fn is_uncertain(row: &Value) -> bool {
    let extra = row.get("extra").and_then(Value::as_object);
    extra
        .map(|e| {
            e.get("cost_unknown")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || e.get("charge_uncertain")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Summarize usage by provider, operation, and day (`report`). `since` is an inclusive
/// `YYYY-MM-DD` date; uncertain receipts are kept out of cost totals under `uncertain`.
pub fn report(host: &dyn Host, since: Option<&str>) -> Value {
    let rows: Vec<Value> = read_all(host)
        .into_iter()
        .filter(|r| {
            since
                .map(|s| {
                    r.get("at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .get(..10)
                        .unwrap_or("")
                        >= s
                })
                .unwrap_or(true)
        })
        .collect();
    let uncertain: Vec<Value> = rows.iter().filter(|r| is_uncertain(r)).cloned().collect();
    let measured: Vec<&Value> = rows.iter().filter(|r| !is_uncertain(r)).collect();

    // Groups keep first-appearance order, and the printed report does too.
    let mut by_source: IndexMap<String, IndexMap<String, f64>> = IndexMap::new();
    let mut by_operation: IndexMap<String, IndexMap<String, f64>> = IndexMap::new();
    let mut by_day: IndexMap<String, IndexMap<String, f64>> = IndexMap::new();

    for row in measured {
        let unit = row.get("unit").and_then(Value::as_str).unwrap_or("limits");
        let cost = row.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        let source = row.get("source").and_then(Value::as_str).unwrap_or("?");
        let operation = row.get("operation").and_then(Value::as_str).unwrap_or("?");
        let day = row
            .get("at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .get(..10)
            .unwrap_or("")
            .to_owned();
        *by_source
            .entry(source.to_owned())
            .or_default()
            .entry(unit.to_owned())
            .or_default() += cost;
        *by_operation
            .entry(format!("{source}.{operation}"))
            .or_default()
            .entry(unit.to_owned())
            .or_default() += cost;
        *by_day
            .entry(day)
            .or_default()
            .entry(unit.to_owned())
            .or_default() += cost;
    }

    let nested = |m: IndexMap<String, IndexMap<String, f64>>| {
        m.into_iter()
            .map(|(k, v)| {
                (
                    k,
                    Value::Object(v.into_iter().map(|(k2, v2)| (k2, json!(v2))).collect()),
                )
            })
            .collect::<Map<String, Value>>()
    };

    json!({
        "ok": true,
        "calls": rows.len(),
        "since": since,
        "by_source": Value::Object(nested(by_source)),
        "by_operation": Value::Object(nested(by_operation)),
        "by_day": Value::Object(nested(by_day)),
        "uncertain": uncertain,
        "log": log_path(host).display().to_string(),
    })
}

/// Paid task IDs so callers can fetch results without paying again (`paid_task_ids`).
pub fn paid_task_ids(host: &dyn Host, source: &str) -> Vec<Value> {
    read_all(host)
        .into_iter()
        .filter(|r| r.get("source").and_then(Value::as_str) == Some(source))
        .filter_map(|r| r.get("task_id").cloned())
        .filter(|t| !t.is_null())
        .collect()
}

/// Column names of the CSV export, in order.
pub const CSV_COLUMNS: [&str; 8] = [
    "at",
    "source",
    "operation",
    "cost",
    "unit",
    "items",
    "task_id",
    "uncertain",
];

/// One flat row per journal entry, in journal order (`iter_rows`).
pub fn iter_rows(host: &dyn Host, since: Option<&str>) -> Vec<Value> {
    read_all(host)
        .into_iter()
        .filter(|row| {
            since
                .map(|s| {
                    row.get("at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .get(..10)
                        .unwrap_or("")
                        >= s
                })
                .unwrap_or(true)
        })
        .map(|row| {
            let cost = row.get("cost").cloned().unwrap_or(Value::Null);
            let task_id = row.get("task_id").cloned().unwrap_or(Value::Null);
            json!({
                "at": row.get("at").and_then(Value::as_str).unwrap_or(""),
                "source": row.get("source").and_then(Value::as_str).unwrap_or(""),
                "operation": row.get("operation").and_then(Value::as_str).unwrap_or(""),
                "cost": cost,
                "unit": row.get("unit").and_then(Value::as_str).unwrap_or("limits"),
                "items": row.get("items").and_then(Value::as_i64).unwrap_or(0),
                "task_id": if task_id.is_null() { json!("") } else { task_id },
                "uncertain": is_uncertain(&row),
            })
        })
        .collect()
}

/// Write `iter_rows` to `path` as CSV with `;` delimiter and UTF-8 BOM (`write_csv`).
pub fn write_csv(host: &dyn Host, path: &Path, since: Option<&str>) -> std::io::Result<usize> {
    let mut out = String::from("\u{feff}");
    let mut write_row = |cells: &[String]| {
        // Semicolon-delimited CSV: quote cells that contain the delimiter, a quote or a newline; double internal quotes.
        let mut first = true;
        for cell in cells {
            if !first {
                out.push(';');
            }
            first = false;
            let needs_quote =
                cell.contains(';') || cell.contains('"') || cell.contains(['\n', '\r']);
            if needs_quote {
                out.push('"');
                for ch in cell.chars() {
                    if ch == '"' {
                        out.push('"');
                    }
                    out.push(ch);
                }
                out.push('"');
            } else {
                out.push_str(cell);
            }
        }
        out.push_str("\r\n");
    };
    write_row(&CSV_COLUMNS.map(str::to_owned));
    let mut count = 0usize;
    for row in iter_rows(host, since) {
        let cells: Vec<String> = CSV_COLUMNS
            .iter()
            .map(|c| {
                let v = row.get(*c).cloned().unwrap_or(Value::Null);
                match &v {
                    Value::String(s) => spiderhead_core::tabular::neutralize_formula(s),
                    other => neutralize_cell(other),
                }
            })
            .collect();
        write_row(&cells);
        count += 1;
    }
    fs::write(path, out)?;
    Ok(count)
}

/// Neutralize spreadsheet formulas, then write the value as text: booleans as `True`/`False`,
/// `None` as an empty cell, numbers in their repr form.
fn neutralize_cell(value: &Value) -> String {
    match value {
        Value::String(s) => spiderhead_core::tabular::neutralize_formula(s),
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => crate::pyjson::float_repr_or_int(n),
        other => crate::pyjson::str_value(other).unwrap_or_else(|| other.to_string()),
    }
}
