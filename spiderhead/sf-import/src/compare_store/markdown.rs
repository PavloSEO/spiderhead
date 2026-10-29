//! The bounded Markdown summary rendered from a complete compare.v2 package —
//! the human report behind the `compare-report` surface.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::CompareError;

use super::export::iter_compare_rows;
use super::shared::{py_json, BUCKETS};

/// The fixed bucket order of the summary table, title-cased.
fn bucket_title(bucket: &str) -> &'static str {
    match bucket {
        "entered" => "Entered",
        "left" => "Left",
        "appeared" => "Appeared",
        "disappeared" => "Disappeared",
        _ => "Unchanged",
    }
}

/// reference-style rendering of a scalar the manifest may carry in place of a
/// number: integers and strings as-is, everything else via canonical JSON.
fn display_value(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => py_json(other),
    }
}

/// A signed delta rendered like the reference `{value:+}` format.
fn display_signed(delta: i64) -> String {
    if delta < 0 {
        format!("{delta}")
    } else {
        format!("+{delta}")
    }
}

fn count_of(value: Option<&Value>) -> Option<i64> {
    // Exact integers only: booleans and floats are rejected like `type(x) is int`.
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        _ => None,
    }
}

fn mapping_of(value: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// Render a bounded summary report from a complete compare.v2 package.
pub fn compare_markdown(manifest_path: &Path, max_bytes: i64) -> Result<String, CompareError> {
    if !(4096..=65_536).contains(&max_bytes) {
        return Err(CompareError("max_bytes must be 4096..65536".into()));
    }
    let manifest_text = std::fs::read_to_string(manifest_path)
        .map_err(|error| CompareError(format!("could not read comparison manifest: {error}")))?;
    let manifest: Value = serde_json::from_str(&manifest_text)
        .map_err(|error| CompareError(format!("could not read comparison manifest: {error}")))?;
    let manifest = manifest
        .as_object()
        .filter(|manifest| {
            manifest.get("schema_version").and_then(Value::as_str) == Some(super::COMPARE_V2)
        })
        .ok_or_else(|| CompareError("unsupported comparison manifest schema".into()))?;
    let summary = mapping_of(manifest.get("summary"))
        .ok_or_else(|| CompareError("comparison manifest is missing summary counts".into()))?;
    let conservation = mapping_of(manifest.get("conservation"))
        .filter(|block| block.get("state").and_then(Value::as_str) == Some("complete"))
        .ok_or_else(|| {
            CompareError("comparison manifest does not prove complete row conservation".into())
        })?;
    let empty = serde_json::Map::new();
    let before = mapping_of(manifest.get("before")).unwrap_or(&empty);
    let after = mapping_of(manifest.get("after")).unwrap_or(&empty);

    let scan_id = |block: &serde_json::Map<String, Value>| match block.get("scan_uuid") {
        Some(value) => display_value(Some(value)),
        None => "unknown".to_owned(),
    };
    let mut lines: Vec<String> = vec![
        "# Saved scan comparison".into(),
        String::new(),
        format!("Before scan: `{}`", scan_id(before)),
        format!("After scan: `{}`", scan_id(after)),
        String::new(),
        "| Change | Findings |".into(),
        "|---|---:|".into(),
    ];
    for bucket in BUCKETS {
        let count = count_of(summary.get(bucket)).filter(|count| *count >= 0);
        let Some(count) = count else {
            return Err(CompareError(format!(
                "comparison summary has an invalid {bucket} count"
            )));
        };
        lines.push(format!("| {} | {count} |", bucket_title(bucket)));
    }
    lines.push(String::new());
    lines.push(format!(
        "Conservation: `{}`.",
        display_value(conservation.get("state"))
    ));
    if let Some(segments) = mapping_of(manifest.get("segment_breakdown")) {
        lines.push(String::new());
        lines.push(format!(
            "Segment breakdown: `{}`.",
            match segments.get("state") {
                Some(value) => display_value(Some(value)),
                None => "unavailable".to_owned(),
            }
        ));
        let segment_list: BTreeMap<String, Value> = segments
            .get("segments")
            .and_then(Value::as_object)
            .map(|segments| {
                segments
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (name, values) in &segment_list {
            let Some(values) = values.as_object() else {
                continue;
            };
            if values.get("state").and_then(Value::as_str) != Some("compared") {
                continue;
            }
            let (Some(segment_before), Some(segment_after), Some(changes)) = (
                mapping_of(values.get("before")),
                mapping_of(values.get("after")),
                mapping_of(values.get("changes")),
            ) else {
                continue;
            };
            let (Some(before_pages), Some(after_pages), Some(before_issues), Some(after_issues)) = (
                count_of(segment_before.get("pages")),
                count_of(segment_after.get("pages")),
                count_of(segment_before.get("issues")),
                count_of(segment_after.get("issues")),
            ) else {
                continue;
            };
            let buckets = BUCKETS
                .iter()
                .map(|bucket| {
                    format!(
                        "{bucket} {}",
                        changes
                            .get(*bucket)
                            .and_then(|value| count_of(Some(value)))
                            .unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- {name}: pages {before_pages} → {after_pages} ({}); findings {before_issues} → {after_issues} ({}). Change buckets: {buckets}.",
                display_signed(after_pages - before_pages),
                display_signed(after_issues - before_issues),
            ));
        }
    }

    let mut by_check: Vec<Obj> = Vec::new();
    for row in iter_compare_rows(manifest_path, "by_check") {
        by_check.push(
            row?.as_object()
                .ok_or_else(|| CompareError("comparison file contains a non-object row".into()))?
                .clone(),
        );
        if by_check.len() > 500 {
            return Err(CompareError("report exceeds the 500-check limit".into()));
        }
    }
    if !by_check.is_empty() {
        lines.push(String::new());
        lines.push("## Findings by check".into());
        lines.push(String::new());
        lines.push("| Check | Entered | Left | Appeared | Disappeared |".into());
        lines.push("|---|---:|---:|---:|---:|".into());
        for row in &by_check {
            let check = display_value(row.get("check")).replace('|', "\\|");
            lines.push(format!(
                "| {check} | {} | {} | {} | {} |",
                display_value(row.get("entered")),
                display_value(row.get("left")),
                display_value(row.get("appeared")),
                display_value(row.get("disappeared")),
            ));
        }
    }
    if let Some(warnings) = manifest.get("warnings").and_then(Value::as_array) {
        if !warnings.is_empty() {
            lines.push(String::new());
            lines.push("## Comparison caveats".into());
            lines.push(String::new());
            for warning in warnings.iter().take(50) {
                lines.push(format!(
                    "- {}",
                    display_value(Some(warning)).replace('\n', " ")
                ));
            }
        }
    }
    let report = format!("{}\n", lines.join("\n"));
    if report.len() as i64 > max_bytes {
        return Err(CompareError("report exceeds max_bytes".into()));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use super::super::export::export_one;

    fn build_manifest(dir: &Path) -> std::path::PathBuf {
        let by_check_rows = vec![
            json!({"check": "missing_title", "entered": 3, "left": 1, "appeared": 0, "disappeared": 0}),
            json!({"check": "bro|ken", "entered": 0, "left": 0, "appeared": 2, "disappeared": 1}),
        ];
        let by_check = export_one(
            dir,
            "by_check",
            &mut by_check_rows.into_iter().map(Ok),
            "none",
            None,
            None,
        )
        .expect("export");
        let manifest = dir.join("compare.json");
        std::fs::write(
            &manifest,
            serde_json::to_string(&json!({
                "schema_version": super::super::COMPARE_V2,
                "before": {"scan_uuid": "aaa"},
                "after": {"scan_uuid": "bbb"},
                "summary": {"entered": 3, "left": 1, "appeared": 2, "disappeared": 1, "unchanged": 40},
                "conservation": {"state": "complete"},
                "segment_breakdown": {
                    "state": "compared",
                    "segments": {
                        "docs": {
                            "state": "compared",
                            "before": {"pages": 10, "issues": 4},
                            "after": {"pages": 12, "issues": 5},
                            "changes": {"entered": 3, "left": 1},
                        }
                    }
                },
                "warnings": ["ignored\nrobots rules"],
                "files": {"by_check": by_check},
            }))
            .expect("json"),
        )
        .expect("write");
        manifest
    }

    #[test]
    fn renders_summary_segment_and_caveats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_manifest(dir.path());
        let report = compare_markdown(&manifest, 65_536).expect("report");
        assert!(report.starts_with("# Saved scan comparison\n"));
        assert!(report.contains("Before scan: `aaa`"));
        assert!(report.contains("After scan: `bbb`"));
        assert!(report.contains("| Entered | 3 |"));
        assert!(report.contains("| Unchanged | 40 |"));
        assert!(report.contains("Conservation: `complete`."));
        assert!(report.contains("Segment breakdown: `compared`."));
        assert!(report.contains(
            "- docs: pages 10 → 12 (+2); findings 4 → 5 (+1). Change buckets: entered 3, left 1, appeared 0, disappeared 0, unchanged 0."
        ));
        assert!(report.contains("## Findings by check"));
        assert!(report.contains("| bro\\|ken | 0 | 0 | 2 | 1 |"));
        assert!(report.contains("## Comparison caveats"));
        assert!(report.contains("- ignored robots rules"));
    }

    #[test]
    fn incomplete_conservation_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_manifest(dir.path());
        let mut broken: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).expect("read")).expect("json");
        broken["conservation"]["state"] = json!("partial");
        std::fs::write(&manifest, serde_json::to_string(&broken).expect("json")).expect("write");
        assert_eq!(
            compare_markdown(&manifest, 65_536).err().map(|e| e.0),
            Some("comparison manifest does not prove complete row conservation".into())
        );
        let mut no_summary: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).expect("read")).expect("json");
        no_summary["conservation"]["state"] = json!("complete");
        no_summary.as_object_mut().expect("obj").remove("summary");
        std::fs::write(&manifest, serde_json::to_string(&no_summary).expect("json"))
            .expect("write");
        assert_eq!(
            compare_markdown(&manifest, 65_536).err().map(|e| e.0),
            Some("comparison manifest is missing summary counts".into())
        );
    }

    #[test]
    fn invalid_counts_and_max_bytes_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_manifest(dir.path());
        let mut broken: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).expect("read")).expect("json");
        broken["summary"]["left"] = json!(-1);
        std::fs::write(&manifest, serde_json::to_string(&broken).expect("json")).expect("write");
        assert_eq!(
            compare_markdown(&manifest, 65_536).err().map(|e| e.0),
            Some("comparison summary has an invalid left count".into())
        );
        // The range check runs before the manifest is even read (4096 itself is valid).
        let report = compare_markdown(&manifest, 4095);
        assert_eq!(
            report.err().map(|e| e.0),
            Some("max_bytes must be 4096..65536".into())
        );
    }
}
