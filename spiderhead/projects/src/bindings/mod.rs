//! Manual project-source bindings: which provider resource IDs a project uses
//!
//! Bindings are local ID records in `sources.json` beside `project.json`. They carry
//! no credentials, no service-account paths, and no provider responses, and nothing here
//! touches the network. `project.json` is never rewritten, so existing projects keep
//! opening unchanged.
//!
//! The readiness view implements the read-only halves of the source status and
//! Cloudflare zone listing locally, because the owning crates are not available yet.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::path::Path;

use jiff::{civil::Date, Span, Zoned};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::error::{value, ProjectsResult};
use crate::workspace::load as workspace_load;

mod source_readiness;
pub use source_readiness::*;

/// Bindings filename.
pub const SOURCES_FILE: &str = "sources.json";
/// Document format.
pub const SOURCES_FORMAT: &str = "spiderhead.project.sources.v1";
/// Document version.
pub const SOURCES_VERSION: i64 = 1;
/// Provider services a binding may name.
pub const SERVICES: &[&str] = &[
    "gsc",
    "ga4",
    "gtm",
    "metrika",
    "webmaster",
    "bing",
    "topvisor",
    "cloudflare",
];
/// Maximum stored bindings.
pub const MAX_BINDINGS: usize = 200;
const MAX_BYTES: u64 = 262_144;
const MAX_RESOURCE: usize = 512;
const MAX_LABEL: usize = 128;
const BINDING_KEYS: &[&str] = &["service", "resource", "label", "origin", "linked_at"];

/// Text form of a JSON scalar; objects and arrays are rejected by date parsing anyway.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Null => "None".to_owned(),
        Value::Number(n) => n.to_string(),
        other => crate::pyjson::dumps_compact(other),
    }
}

fn control_chars(s: &str) -> bool {
    s.chars()
        .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
}

fn resource(raw: &Value) -> ProjectsResult<String> {
    let Some(text) = raw.as_str() else {
        return Err(value(
            "resource must be a bounded, trimmed ID without control characters",
        ));
    };
    if text.is_empty()
        || text.chars().count() > MAX_RESOURCE
        || control_chars(text)
        || text != text.trim()
    {
        return Err(value(
            "resource must be a bounded, trimmed ID without control characters",
        ));
    }
    Ok(text.to_owned())
}

fn label(raw: &Value) -> ProjectsResult<Value> {
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let Some(text) = raw.as_str() else {
        return Err(value("label must be a bounded human label"));
    };
    if text.trim().is_empty() || text.chars().count() > MAX_LABEL {
        return Err(value("label must be a bounded human label"));
    }
    if control_chars(text) {
        return Err(value("label must not contain control characters"));
    }
    Ok(json!(text))
}

fn service(raw: &Value) -> ProjectsResult<String> {
    match raw.as_str() {
        Some(s) if SERVICES.contains(&s) => Ok(s.to_owned()),
        _ => Err(value(format!(
            "unknown service; expected one of: {}",
            SERVICES.join(", ")
        ))),
    }
}

fn read_bindings(root: &Path) -> ProjectsResult<Vec<Value>> {
    let path = root.join(SOURCES_FILE);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(Vec::new());
    }
    if path.is_symlink() || !path.is_file() || fs::metadata(&path)?.len() > MAX_BYTES {
        return Err(value("sources.json is unsafe or exceeds its byte limit"));
    }
    let document: Value = (|| {
        let text =
            fs::read_to_string(&path).map_err(|_| value("sources.json is not valid JSON"))?;
        serde_json::from_str(&text).map_err(|_| value("sources.json is not valid JSON"))
    })()?;
    let Some(obj) = document.as_object() else {
        return Err(value("sources.json has an unsupported shape"));
    };
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    if keys != ["format", "version", "bindings"].into_iter().collect()
        || document["format"].as_str() != Some(SOURCES_FORMAT)
        || document["version"].as_i64() != Some(SOURCES_VERSION)
        || !document["bindings"].is_array()
        || document["bindings"].as_array().map_or(0, Vec::len) > MAX_BINDINGS
    {
        return Err(value("sources.json has an unsupported shape"));
    }
    let bindings = document["bindings"].as_array().cloned().unwrap_or_default();
    for item in &bindings {
        let keys: BTreeSet<&str> = item
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if keys != BINDING_KEYS.iter().copied().collect() {
            return Err(value("sources.json binding has an unsupported shape"));
        }
        service(&item["service"])?;
        resource(&item["resource"])?;
        label(&item["label"])?;
    }
    Ok(bindings)
}

fn write_bindings(root: &Path, bindings: &[Value]) -> ProjectsResult<()> {
    let target = root.join(SOURCES_FILE);
    if target.is_symlink() {
        return Err(value("sources.json must not be a symlink"));
    }
    let text = crate::pyjson::dumps_indent2(&json!({
        "format": SOURCES_FORMAT,
        "version": SOURCES_VERSION,
        "bindings": bindings,
    }));
    let staged = root.join(".sources.json.stage");
    let _ = fs::remove_file(&staged);
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        spiderhead_core::filesystem::PrivateMode::private_mode(&mut options, 0o600);
        use std::io::Write;
        let mut stream = options.open(&staged)?;
        stream.write_all(text.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.sync_all()?;
    }
    fs::rename(&staged, &target)?;
    crate::locks::fsync_directory(root)?;
    let _ = fs::remove_file(&staged);
    Ok(())
}

fn view(binding: &Value) -> Value {
    let mut out = binding.as_object().cloned().unwrap_or_default();
    out.insert("status".into(), json!("selected"));
    Value::Object(out)
}

// --- local read-only equivalents of data_sources.sources_db / cloudflare_history ---

struct SourceSpec {
    table: &'static str,
    dimensions: &'static [&'static str],
    metrics: &'static [&'static str],
    lag_days: i64,
    non_additive: &'static [&'static str],
    date_timezone: &'static str,
}

const SOURCES_META_KIND: &str = "spiderhead.sources";
const SOURCES_SCHEMA_VERSION: &str = "1";
const CLOUDFLARE_FORMAT: &str = "spiderhead.cloudflare-history.v1";

fn source_spec(source: &str) -> SourceSpec {
    match source {
        "gsc" => SourceSpec {
            table: "gsc",
            dimensions: &["query", "page"],
            metrics: &["clicks", "impressions", "ctr", "position"],
            lag_days: 3,
            non_additive: &["ctr", "position"],
            date_timezone: "America/Los_Angeles",
        },
        "ga4" => SourceSpec {
            table: "ga4",
            dimensions: &["landing_page", "channel"],
            metrics: &["sessions", "engaged_sessions", "users"],
            lag_days: 1,
            non_additive: &["users"],
            date_timezone: "provider_resource",
        },
        "metrika" => SourceSpec {
            table: "metrika",
            dimensions: &["traffic_source", "landing_page"],
            metrics: &["visits", "users", "bounce_rate"],
            lag_days: 1,
            non_additive: &["users", "bounce_rate"],
            date_timezone: "provider_resource",
        },
        "webmaster" => SourceSpec {
            table: "webmaster",
            dimensions: &["query"],
            metrics: &["shows", "clicks", "show_position", "click_position"],
            lag_days: 3,
            non_additive: &["show_position", "click_position"],
            date_timezone: "provider_resource",
        },
        "webmaster_history" => SourceSpec {
            table: "webmaster_history",
            dimensions: &["indicator"],
            metrics: &["value"],
            lag_days: 3,
            non_additive: &["value"],
            date_timezone: "provider_resource",
        },
        _ => SourceSpec {
            table: "",
            dimensions: &[],
            metrics: &[],
            lag_days: 0,
            non_additive: &[],
            date_timezone: "provider_resource",
        },
    }
}

fn date_iso(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn parse_date(text: &str) -> Option<Date> {
    text.parse::<Date>().ok()
}

/// `available_end(source, as_of=…)`.
fn available_end(source: &str, as_of: Option<Date>) -> ProjectsResult<Date> {
    let spec = source_spec(source);
    let mut today = match as_of {
        Some(d) => d,
        None => {
            if spec.date_timezone == "America/Los_Angeles" {
                Zoned::now()
                    .with_time_zone(crate::pytime::zone("America/Los_Angeles")?)
                    .datetime()
                    .date()
            } else {
                Zoned::now().datetime().date()
            }
        }
    };
    if spec.date_timezone != "America/Los_Angeles" {
        today = today
            .checked_sub(Span::new().days(1))
            .map_err(|e| value(e.to_string()))?;
    }
    today
        .checked_sub(Span::new().days(spec.lag_days))
        .map_err(|e| value(e.to_string()))
}

fn days_between(start: Date, end: Date) -> Vec<String> {
    let mut days = Vec::new();
    let mut current = start;
    while current <= end {
        days.push(date_iso(current));
        match current.checked_add(Span::new().days(1)) {
            Ok(next) => current = next,
            Err(_) => break,
        }
    }
    days
}

fn gaps(days: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for pair in days.windows(2) {
        let before = parse_date(&pair[0]);
        let after = parse_date(&pair[1]);
        if let (Some(b), Some(a)) = (before, after) {
            if (a - b).get_days() > 1 {
                out.push(json!([
                    date_iso(b.checked_add(Span::new().days(1)).unwrap_or(b)),
                    date_iso(a.checked_sub(Span::new().days(1)).unwrap_or(a)),
                ]));
            }
        }
    }
    out
}

/// `sync_log` row: `(source, resource, date, rows, state, fetched_at, last_attempt_state)`.
type SyncRow = (
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
);

/// Read-only `sources_db.connect(create=False)` + `status(path)`.
fn sources_status(path: &Path) -> ProjectsResult<Vec<Value>> {
    if !path.is_file() {
        return Err(value(format!(
            "no sources database at {}; run sources-sync first",
            path.display()
        )));
    }
    let con = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| value(e.to_string()))?;
    con.execute_batch("BEGIN")
        .map_err(|e| value(e.to_string()))?;
    let marker: bool = con
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sources_meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if marker {
        let mut stmt = con
            .prepare("SELECT key, value FROM sources_meta")
            .map_err(|e| value(e.to_string()))?;
        let identity: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| value(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        let kind = identity
            .iter()
            .find(|(k, _)| k == "kind")
            .map(|(_, v)| v.as_str());
        let version = identity
            .iter()
            .find(|(k, _)| k == "schema_version")
            .map(|(_, v)| v.as_str());
        if kind != Some(SOURCES_META_KIND) || version != Some(SOURCES_SCHEMA_VERSION) {
            return Err(value(
                "unsupported sources database identity or schema version",
            ));
        }
        con.execute_batch("PRAGMA query_only=ON")
            .map_err(|e| value(e.to_string()))?;
    } else {
        return Err(value(
            "existing database is not a versioned sources database; back it up first",
        ));
    }
    let mut ledger: Vec<SyncRow> = Vec::new();
    {
        let mut stmt = con
            .prepare(
                "SELECT source, resource, date, rows, state, fetched_at, last_attempt_state \
                 FROM sync_log ORDER BY source, resource, date",
            )
            .map_err(|e| value(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| value(e.to_string()))?;
        for row in rows.flatten() {
            ledger.push(row);
        }
    }
    let mut entries: Vec<Value> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for (source, resource, _, _, _, _, _) in &ledger {
        let key = (source.clone(), resource.clone());
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    for (source, resource) in seen {
        let days: Vec<_> = ledger
            .iter()
            .filter(|(s, r, _, _, _, _, _)| s == &source && r == &resource)
            .collect();
        let spec = source_spec(&source);
        let stored: i64 = con
            .query_row(
                &format!("SELECT COUNT(*) FROM {} WHERE resource = ?", spec.table),
                [resource.as_str()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let dates: Vec<String> = days.iter().map(|d| d.2.clone()).collect();
        let mut coverage = Map::new();
        for state in [
            "complete", "empty", "partial", "failed", "pending", "lagged",
        ] {
            coverage.insert(
                state.to_owned(),
                json!(days
                    .iter()
                    .filter(|d| d.4 == state)
                    .map(|d| d.2.clone())
                    .collect::<Vec<_>>()),
            );
        }
        let requested: BTreeSet<String> = dates.iter().cloned().collect();
        let first = dates.first().and_then(|d| parse_date(d));
        let last = dates.last().and_then(|d| parse_date(d));
        let unrequested: Vec<String> = match (first, last) {
            (Some(f), Some(l)) => days_between(f, l)
                .into_iter()
                .filter(|d| !requested.contains(d))
                .collect(),
            _ => Vec::new(),
        };
        coverage.insert("unrequested".into(), json!(unrequested));
        let gap_list = gaps(&dates);
        let synced = coverage["complete"].as_array().map_or(0, Vec::len)
            + coverage["empty"].as_array().map_or(0, Vec::len);
        let last_fetched = days.iter().filter_map(|d| d.5.clone()).max();
        let mut last_attempts = Map::new();
        for d in &days {
            if let Some(attempt) = &d.6 {
                if attempt != "complete" && attempt != "empty" {
                    last_attempts.insert(d.2.clone(), json!(attempt));
                }
            }
        }
        entries.push(json!({
            "source": source,
            "resource": resource,
            "schema_version": SOURCES_SCHEMA_VERSION,
            "dimensions": spec.dimensions,
            "metrics": spec.metrics,
            "non_additive_metrics": spec.non_additive,
            "date_timezone": spec.date_timezone,
            "lag_cutoff_policy": if spec.date_timezone == "America/Los_Angeles" {
                "resource_pacific_date"
            } else {
                "conservative_utc_minus_one_day"
            },
            "rows": stored,
            "first_date": dates.first(),
            "last_date": dates.last(),
            "requested_days": days.len(),
            "synced_days": synced,
            "empty_days": coverage["empty"].as_array().map_or(0, Vec::len),
            "incomplete_days": coverage["partial"],
            "coverage": coverage,
            "gaps": gap_list,
            "last_fetched_at": last_fetched,
            "last_attempts": last_attempts,
        }));
    }
    Ok(entries)
}
