//! Denominator-backed scan summary using only the bounded project observation.
//!
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::error::{TuiError, TuiResult};
use crate::labels::label;
use crate::localization::{number, ui};
use crate::pyjson::{self, OrPy};
use crate::text::Text;
use crate::theme::Palette;

/// `project_directory`: accept either the project root or its parent folder
/// (`<dir>/project/project.json` layout).
pub fn project_directory(directory: &str) -> String {
    let path = expand_tilde(directory);
    if !path.join("project.json").is_file() && path.join("project/project.json").is_file() {
        return path.join("project").to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

fn expand_tilde(directory: &str) -> PathBuf {
    if let Some(rest) = directory.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if directory == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    // `Path(directory).expanduser()` also handles `~user/...`; only `~` and
    // `~/` are reachable from the CLI flag, so leave other forms untouched.
    normalize_separators(directory)
}

fn normalize_separators(directory: &str) -> PathBuf {
    Path::new(directory)
        .components()
        .fold(PathBuf::new(), |mut acc, part| {
            if part == Component::CurDir && acc.as_os_str().is_empty() {
                acc.push(".");
            } else {
                acc.push(part.as_os_str());
            }
            acc
        })
}

/// `select`: keep only scans/runs whose uuid/id/scan_uuid/run_id equals `scan`;
/// raises `MissingScan` when none match.
pub fn select(snapshot: &Value, scan: Option<&str>) -> TuiResult<Value> {
    let scan = match scan {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(snapshot.clone()),
    };
    let matches = |item: &Value| {
        ["uuid", "id", "scan_uuid", "run_id"]
            .iter()
            .any(|k| pyjson::get(item, k).map(pyjson::py_str).as_deref() == Some(scan))
    };
    let mut result = snapshot.clone();
    let scans: Vec<Value> = pyjson::path(&result, &["scans", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| matches(item))
        .collect();
    let runs: Vec<Value> = pyjson::path(&result, &["runs", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| matches(item))
        .collect();
    if scans.is_empty() && runs.is_empty() {
        return Err(TuiError::MissingScan);
    }
    result
        .as_object_mut()
        .unwrap()
        .entry("scans")
        .or_insert(Value::Null)["items"] = Value::Array(scans);
    result
        .as_object_mut()
        .unwrap()
        .entry("runs")
        .or_insert(Value::Null)["items"] = Value::Array(runs);
    result["selected_scan"] = Value::String(scan.to_string());
    Ok(result)
}

/// `summary`: one-line progress for the header — never invents unknown counts.
pub fn summary(snapshot: Option<&Value>) -> String {
    let Some(snapshot) = snapshot else {
        return ui("Loading…");
    };
    let runs = pyjson::path(snapshot, &["runs", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let run = runs
        .iter()
        .find(|r| pyjson::str_of(r, "state") == "running")
        .or_else(|| runs.first());
    let scans = pyjson::path(snapshot, &["scans", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let saved = scans.first();
    if run.is_none() && saved.is_none() {
        return ui("No scan recorded");
    }
    let empty = Value::Object(Default::default());
    let run = run.unwrap_or(&empty);
    let saved = saved.unwrap_or(&empty);
    let frontier = pyjson::path(saved, &["evidence", "frontier", "counts"])
        .cloned()
        .unwrap_or(Value::Null);
    let counts = if pyjson::truthy(pyjson::get(run, "counters").unwrap_or(&Value::Null)) {
        pyjson::get(run, "counters").cloned().unwrap()
    } else {
        let mut c = serde_json::json!({"fetched": pyjson::get(&frontier, "done").cloned().unwrap_or(Value::Null)});
        if let Value::Object(map) = &frontier {
            for (k, v) in map {
                c[k] = v.clone();
            }
        }
        c
    };
    let fetched = pyjson::get(&counts, "fetched")
        .cloned()
        .unwrap_or(Value::Null);
    let queued = pyjson::get(&counts, "queued")
        .cloned()
        .unwrap_or(Value::Null);
    let inflight = pyjson::get(&counts, "inflight")
        .cloned()
        .unwrap_or(Value::Null);
    let semantics = pyjson::path(run, &["telemetry", "queue_semantics"])
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if pyjson::truthy(&frontier) {
                "separate".into()
            } else {
                "unknown".into()
            }
        });
    let mut population = vec![fetched.clone(), queued.clone()];
    if semantics == "separate" {
        population.push(inflight.clone());
    }
    let total: Option<i64> = if semantics != "unknown" && population.iter().all(pyjson::is_int) {
        Some(population.iter().filter_map(Value::as_i64).sum())
    } else {
        None
    };
    let progress = if let (Some(total), Some(f)) = (total, fetched.as_i64()) {
        if total != 0 {
            format!(
                "{} / {} · {:.0}%",
                number(f),
                number(total),
                f as f64 / total as f64 * 100.0
            )
        } else {
            number(f)
        }
    } else if let Some(f) = fetched.as_i64() {
        number(f)
    } else {
        ui("Not measured")
    };
    let unknown = ui("unknown");
    let mode = pyjson::path(run, &["collector", "mode"])
        .and_then(Value::as_str)
        .or_else(|| pyjson::get(saved, "mode").and_then(Value::as_str));
    let state_label = label(
        pyjson::get(run, "state")
            .or_py(|| pyjson::get(saved, "lifecycle"))
            .unwrap_or(&Value::Null),
    );
    let freshness = label(pyjson::path(run, &["telemetry", "state"]).unwrap_or(&Value::Null));
    let errors = pyjson::get(&counts, "errors")
        .filter(|v| !v.is_null())
        .or_else(|| pyjson::get(run, "errors"))
        .cloned()
        .unwrap_or(Value::Null);
    let errors_text = if errors.is_number() {
        pyjson::py_str(&errors)
    } else {
        unknown.clone()
    };
    let run_id = pyjson::get(run, "id")
        .or_py(|| pyjson::get(run, "uuid"))
        .or_py(|| pyjson::get(saved, "uuid"))
        .cloned()
        .unwrap_or(Value::Null);
    let parts: Vec<Option<String>> = vec![
        Some(run_label(&run_id)),
        mode.map(|m| label(&Value::String(m.to_string()))),
        Some(state_label.clone()),
        Some(progress),
        Some(format!(
            "{} {}",
            ui("Queue"),
            queued
                .as_i64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| unknown.clone())
        )),
        Some(format!(
            "{} {}",
            ui("Excluded"),
            pyjson::get(&counts, "excluded")
                .map(|v| {
                    v.as_i64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| pyjson::py_str(v))
                })
                .unwrap_or_else(|| unknown.clone())
        )),
        Some(format!("{} {}", ui("Errors"), errors_text)),
        (freshness != state_label).then_some(freshness),
    ];
    parts
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `run_label`: short run id for people (`r-ad99`); the full hash stays in JSON.
pub fn run_label(identity: &Value) -> String {
    let text = match identity {
        Value::Null => String::new(),
        other => pyjson::py_str(other),
    };
    let digits: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if digits.is_empty() {
        "—".into()
    } else {
        format!("r-{}", &digits[..digits.len().min(4)])
    }
}

/// `panel`: the compact "Scan now" line; a real `Panel` only in full mode.
pub fn panel(
    snapshot: Option<&Value>,
    palette: &Palette,
    compact: bool,
) -> crate::render::Renderable {
    let line = Text::new(
        &summary(snapshot),
        if palette.color { &palette.accent } else { "" },
    );
    if compact {
        let site = snapshot
            .and_then(|s| pyjson::path(s, &["project", "site"]))
            .cloned()
            .unwrap_or(Value::Null);
        let label = pyjson::get(&site, "label")
            .or_py(|| pyjson::get(&site, "host"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| ui("Project"));
        return crate::render::Renderable::Text(Text::new(
            &format!("SEOHEAD · {} · {}: {}", label, ui("Scan now"), line.plain),
            "",
        ));
    }
    crate::render::Renderable::Panel(crate::render::Panel {
        renderable: Box::new(crate::render::Renderable::Group(vec![
            crate::render::Renderable::Text(line),
        ])),
        title: Some(ui("Scan now")),
        title_align: "center".into(),
        border_style: if palette.color {
            palette.accent.clone()
        } else {
            "none".into()
        },
        padding: (0, 1, 0, 1),
        height: None,
        expand: true,
    })
}
