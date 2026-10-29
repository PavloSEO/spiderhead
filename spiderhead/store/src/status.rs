//! Read-only operational status summaries for validated scan artifacts.

use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::link_positions::position_summary;
use crate::scan_artifact::open_scan_mode;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Columns of the scan header copied into `source`, in output order.
const SOURCE_KEYS: &[&str] = &[
    "scan_uuid",
    "format_version",
    "source_kind",
    "parent_scan_uuid",
    "writer_version",
    "writer_revision",
    "evidence_revision",
    "created_at",
    "finished_at",
    "lifecycle",
    "finish_reason",
];

/// Current UTC instant as ISO-8601 with a `Z` suffix.
fn utc_now_iso() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let micros = now.subsec_micros();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{fraction}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// What a damaged scan can still say about itself: `(scan_uuid, checkpoint_available)`.
fn probe_damaged(path: &Path) -> (Option<String>, Option<bool>) {
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return (None, None);
    };
    let Ok(con) = Connection::open_with_flags(&resolved, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return (None, None);
    };
    let uuid = con
        .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten();
    let checkpoint = con
        .query_row("SELECT 1 FROM resume_state WHERE singleton=1", [], |_| {
            Ok(())
        })
        .map(|_| true)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(other),
        })
        .ok();
    (uuid, checkpoint)
}

/// The refusal payload for a scan that could not be opened.
fn unopened(path: &Path, error: &StoreError, checked_at: &str) -> Map<String, Value> {
    let (scan_uuid, checkpoint_available) = probe_damaged(path);
    let missing = !path.exists();
    let integrity_state = if missing { "not_checked" } else { "damaged" };
    let text = error.to_string();
    let is_sqlite = matches!(error, StoreError::Sqlite(_));
    let is_scan = matches!(error, StoreError::ScanError(_));
    let reason = if missing {
        "missing_file".to_string()
    } else if is_scan {
        text.clone()
    } else if is_sqlite {
        "SQLite integrity or schema check failed".to_string()
    } else {
        "scan artifact cannot be read".to_string()
    };
    let not_a_database = text.contains("not a database") || text.contains("file is not a database");
    let display = path.to_string_lossy().into_owned();
    let mut out = Map::new();
    if missing || not_a_database || !(is_sqlite || is_scan) {
        // A missing file or a non-database is a one-line refusal (#1402).
        let message = if missing {
            "scan artifact is missing".to_string()
        } else if not_a_database {
            "not a scan database".to_string()
        } else {
            reason
        };
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(message));
        out.insert(
            "state".into(),
            json!(if missing {
                "unavailable"
            } else {
                "not_a_scan_database"
            }),
        );
        out.insert("path".into(), json!(display));
        return out;
    }
    let affected = match (&scan_uuid, integrity_state) {
        (Some(uuid), "damaged") if !uuid.is_empty() => json!([uuid]),
        _ => json!([]),
    };
    out.insert("ok".into(), json!(false));
    out.insert("error".into(), json!(reason));
    out.insert(
        "source".into(),
        match &scan_uuid {
            Some(uuid) if !uuid.is_empty() => json!({"scan_uuid": uuid}),
            _ => Value::Null,
        },
    );
    out.insert(
        "frontier".into(),
        json!({"state": "unavailable", "reason": "scan could not be opened", "counts": null}),
    );
    out.insert(
        "link_positions".into(),
        json!({"state": "unavailable", "reason": "scan could not be opened"}),
    );
    out.insert("committed_page_outcomes".into(), Value::Null);
    out.insert("validation".into(), json!("not_checked"));
    out.insert(
        "integrity".into(),
        json!({
            "state": integrity_state,
            "checked_at": checked_at,
            "damaged_pages": null,
            "affected_scan_ids": affected,
            "reason": reason,
        }),
    );
    out.insert("checkpoint_available".into(), json!(checkpoint_available));
    out
}

/// Summarize one scan snapshot without fetching, retrying, or changing its bytes.
///
/// By default the scan is accepted by a light header/schema check and the response says
/// ``validation: "light"``; ``full_validation=True`` runs the complete artifact validation
/// (``"full"``). Counters come from SQL either way.
/// `full_validation` defaults to false.
pub fn scan_status(
    input_path: &str,
    full_validation: bool,
) -> Result<Map<String, Value>, StoreError> {
    scan_status_with(input_path, full_validation, &|path, light| {
        open_scan_mode(path, false, None, light)
    })
}

/// Opens a scan for [`scan_status_with`]: `(path, light)` -> `(connection, "light" | "full")`.
pub type ScanOpener<'a> = &'a dyn Fn(&Path, bool) -> Result<(Connection, &'static str), StoreError>;

/// [`scan_status`] over an injected opener (the production opener is `open_scan_mode`).
pub fn scan_status_with(
    input_path: &str,
    full_validation: bool,
    opener: ScanOpener<'_>,
) -> Result<Map<String, Value>, StoreError> {
    if input_path.is_empty() {
        return Err(StoreError::Invalid("input_path is required".into()));
    }
    let path = Path::new(input_path);
    let checked_at = utc_now_iso();
    let (con, validation) = match opener(path, !full_validation) {
        Ok(opened) => opened,
        Err(error) => return Ok(unopened(path, &error, &checked_at)),
    };
    let scan: Map<String, Value> =
        con.query_row("SELECT * FROM scan WHERE singleton=1", [], |row| {
            let mut out = Map::new();
            for (index, name) in row.as_ref().column_names().iter().enumerate() {
                out.insert((*name).to_string(), cell_value(row.get_ref(index)?));
            }
            Ok(out)
        })?;
    let get = |key: &str| scan.get(key).cloned().unwrap_or(Value::Null);
    let truthy = |key: &str| scan.get(key).and_then(Value::as_i64).unwrap_or(0) != 0;
    let mut source = Map::new();
    for key in SOURCE_KEYS {
        source.insert((*key).to_string(), get(key));
    }
    source.insert("crawl_partial".into(), json!(truthy("crawl_partial")));
    source.insert("corpus_partial".into(), json!(truthy("corpus_partial")));

    let mut outcomes = [0i64; 6]; // 2xx 3xx 4xx 5xx other no_response
    {
        let mut stmt =
            con.prepare("SELECT status_code,COUNT(*) FROM pages GROUP BY status_code")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (code, count) = row?;
            match code {
                None => outcomes[5] += count,
                Some(c) if (200..600).contains(&c) => outcomes[(c / 100 - 2) as usize] += count,
                Some(_) => outcomes[4] += count,
            }
        }
    }
    let outcomes = json!({
        "2xx": outcomes[0], "3xx": outcomes[1], "4xx": outcomes[2], "5xx": outcomes[3],
        "other": outcomes[4], "no_response": outcomes[5],
    });
    let frontier = if get("source_kind") == json!("legacy_import") {
        json!({
            "state": "unavailable",
            "reason": "legacy import retains no native frontier",
            "counts": null,
        })
    } else {
        let mut counts = Map::new();
        for state in ["queued", "inflight", "done", "excluded"] {
            counts.insert(state.into(), json!(0));
        }
        let mut stmt = con.prepare("SELECT state,COUNT(*) FROM frontier GROUP BY state")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (state, count) = row?;
            counts.insert(state, json!(count));
        }
        json!({"state": "available", "reason": "", "counts": counts})
    };
    let lifecycle = source
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let has_checkpoint = con
        .query_row("SELECT 1 FROM resume_state WHERE singleton=1", [], |_| {
            Ok(())
        })
        .is_ok();
    let checkpoint_available = matches!(
        lifecycle.as_str(),
        "created" | "running" | "interrupted" | "paused"
    ) && has_checkpoint;
    let full = validation == "full";
    let integrity_reason = if full_validation {
        ""
    } else if full {
        "full validation timestamp is not retained"
    } else {
        "full validation was not requested"
    };
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("source".into(), Value::Object(source));
    out.insert("frontier".into(), frontier);
    out.insert(
        "link_positions".into(),
        Value::Object(position_summary(&con, 100_000)?),
    );
    out.insert("committed_page_outcomes".into(), outcomes);
    out.insert("validation".into(), json!(validation));
    out.insert(
        "integrity".into(),
        json!({
            "state": if full { "ok" } else { "not_checked" },
            "checked_at": if full_validation { json!(checked_at) } else { Value::Null },
            "damaged_pages": if full { json!(0) } else { Value::Null },
            "affected_scan_ids": [],
            "reason": integrity_reason,
        }),
    );
    out.insert("checkpoint_available".into(), json!(checkpoint_available));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_a_one_line_refusal() {
        let dir = std::env::temp_dir().join("w2s6-status-missing");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("absent.sqlite");
        let out = scan_status(path.to_str().unwrap(), false).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("scan artifact is missing"));
        assert_eq!(out["state"], json!("unavailable"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_database_is_a_one_line_refusal() {
        let dir = std::env::temp_dir().join("w2s6-status-garbage");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("garbage.sqlite");
        std::fs::write(
            &path,
            b"this is not a sqlite database at all, just text padding....",
        )
        .unwrap();
        let out = scan_status(path.to_str().unwrap(), false).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["state"], json!("not_a_scan_database"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_stamp_has_z_suffix() {
        let stamp = utc_now_iso();
        assert!(stamp.ends_with('Z') && stamp.len() >= 20);
    }

    #[test]
    fn empty_input_path_is_refused() {
        assert!(scan_status("", false).is_err());
    }
}

/// Shared helpers for comparing read-path output with recorded baselines.
#[cfg(test)]
pub(crate) mod oracle_support {
    use regex::Regex;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// Sites with a recorded baseline and raw scan.
    pub const SITES: &[&str] = &[
        "basic",
        "canonical-hreflang",
        "errors",
        "redirects",
        "robots-sitemap",
        "schema",
    ];

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
    }

    /// The raw (un-normalised) scan of one site.
    pub fn raw_scan(site: &str) -> PathBuf {
        root().join("raw-scans").join(site).join("scan.sqlite")
    }

    /// The `stdout` payload of one recorded baseline file.
    pub fn baseline(site: &str, file: &str) -> Value {
        let text = std::fs::read_to_string(root().join("baselines").join(site).join(file))
            .expect("baseline file");
        let mut value: Value = serde_json::from_str(&text).expect("baseline json");
        value["stdout"].take()
    }

    /// The normalisation rules that these outputs touch
    /// (origin, UUIDs, timestamps); keys are visited in sorted order.
    pub fn normalise(value: &Value) -> Value {
        let origin = Regex::new(r"http://127\.0\.0\.1:\d+").unwrap();
        let port = Regex::new(r"127\.0\.0\.1:\d+").unwrap();
        let uuid = Regex::new(
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        )
        .unwrap();
        let stamp =
            Regex::new(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?$")
                .unwrap();
        let mut seen: Vec<String> = Vec::new();
        fn walk(
            v: &Value,
            seen: &mut Vec<String>,
            rules: (&Regex, &Regex, &Regex, &Regex),
        ) -> Value {
            let (origin, port, uuid, stamp) = rules;
            match v {
                Value::Object(map) => {
                    let mut keys: Vec<&String> = map.keys().collect();
                    keys.sort();
                    let mut out = serde_json::Map::new();
                    for k in keys {
                        out.insert(k.clone(), walk(&map[k], seen, rules));
                    }
                    Value::Object(out)
                }
                Value::Array(items) => {
                    Value::Array(items.iter().map(|i| walk(i, seen, rules)).collect())
                }
                Value::String(s) => {
                    if stamp.is_match(s) {
                        return Value::String("<TS>".into());
                    }
                    let s = origin.replace_all(s, "{BASE}");
                    let s = port.replace_all(&s, "127.0.0.1:{PORT}").into_owned();
                    let replaced = uuid.replace_all(&s, |caps: &regex::Captures<'_>| {
                        let id = caps[0].to_lowercase();
                        let index = seen.iter().position(|x| *x == id).unwrap_or_else(|| {
                            seen.push(id);
                            seen.len() - 1
                        });
                        format!("<UUID:{}>", index + 1)
                    });
                    Value::String(replaced.into_owned())
                }
                other => other.clone(),
            }
        }
        walk(value, &mut seen, (&origin, &port, &uuid, &stamp))
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::oracle_support::*;
    use super::*;

    // Light validation: frontier and HTTP outcome counts are disjoint and need no network.
    #[test]
    fn scan_status_matches_the_reference_baseline() {
        let mut divergences = Vec::new();
        for site in SITES {
            let path = raw_scan(site);
            let actual = scan_status_with(path.to_str().unwrap(), false, &|p, _| {
                crate::link_context::open_light(p)
            })
            .unwrap();
            let actual = normalise(&Value::Object(actual));
            let expected = baseline(site, "scan-status.json");
            if actual != expected {
                divergences.push(format!("{site}:\nexpected {expected}\nactual   {actual}"));
            }
        }
        assert!(divergences.is_empty(), "{}", divergences.join("\n"));
    }
}
