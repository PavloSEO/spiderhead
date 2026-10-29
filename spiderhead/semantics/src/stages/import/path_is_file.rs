//! Import preview and apply: `plan` returns the preview, `run` writes it, and
//! both share `plan_rows` for merge-policy checks and new/existing counts.

use super::*;

pub(super) fn path_is_file(p: &Path) -> bool {
    p.is_file()
}

/// Build the import preview shared by `plan` and `run`: validates the merge policy and counts new and existing phrases.
pub(super) fn plan_rows(
    store: &Store,
    file: &Path,
    sheet: Option<&str>,
    merge: &str,
    rows: &[Value],
    applied: &IndexMap<String, String>,
    input_rows: usize,
) -> Result<Value, crate::Error> {
    if !["add", "update", "replace"].contains(&merge) {
        return Err(err("merge policy must be add, update, or replace"));
    }
    if merge == "replace" && rows.is_empty() {
        return Err(err(
            "replace import contains no valid queries; refusing to clear the semantic core",
        ));
    }
    let current: std::collections::HashSet<String> = store
        .q("SELECT norm FROM phrases", &[])?
        .iter()
        .filter_map(|r| r["norm"].as_str().map(String::from))
        .collect();
    let existing = rows
        .iter()
        .filter(|r| current.contains(r["query"].as_str().unwrap_or_default()))
        .count();
    let new = rows.len() - existing;
    let mut groups = Map::new();
    for item in rows {
        let group = item["values"]
            .get("group_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("(ungrouped)")
            .to_string();
        let n = groups.get(&group).and_then(Value::as_i64).unwrap_or(0);
        groups.insert(group, json!(n + 1));
    }
    let file_type = file
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut applied_obj = Map::new();
    for (k, v) in applied {
        applied_obj.insert(k.clone(), json!(v));
    }
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "mode": "preview",
        "state": "complete",
        "format": "spiderhead.semantics.import-plan.v1",
        "file_type": file_type,
        "sheet": sheet,
        "merge": merge,
        "applied_mapping": Value::Object(applied_obj),
        "input_rows": input_rows,
        "unique_queries": rows.len(),
        "new": if merge != "replace" { new } else { rows.len() },
        "already_in_core": if merge != "replace" { existing } else { 0 },
        "duplicates_in_input": input_rows - rows.len(),
        "group_counts": Value::Object(groups),
        "preview": Value::Array(rows.iter().take(10).cloned().collect()),
        "replacement_rows": if merge == "replace" { json!(current.len()) } else { Value::Null },
    }))
}

/// Dry run of an import: parses the file and returns the preview without writing.
pub fn plan(
    store: &Store,
    _cfg: &Cfg,
    file: &Path,
    sheet: Option<&str>,
    mapping: Option<&Map<String, Value>>,
    merge: &str,
) -> Result<Value, crate::Error> {
    let (rows, applied, input_rows) = parse_file(file, sheet, mapping)?;
    plan_rows(store, file, sheet, merge, &rows, &applied, input_rows)
}

/// Apply an import; with merge `replace` the phrase-related tables are cleared first.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    file: &Path,
    sheet: Option<&str>,
    mapping: Option<&Map<String, Value>>,
    merge: &str,
) -> Result<Value, crate::Error> {
    let (rows, applied, input_rows) = parse_file(file, sheet, mapping)?;
    let preview = plan_rows(store, file, sheet, merge, &rows, &applied, input_rows)?;
    if merge == "replace" {
        for t in ["edges", "serp", "domains", "clusters", "phrases"] {
            store.exec(&format!("DELETE FROM {t}"), &[])?;
        }
        let has_lc = !store
            .q(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='landing_candidates'",
                &[],
            )?
            .is_empty();
        if has_lc {
            store.exec("DELETE FROM landing_candidates", &[])?;
        }
    }
    let mut added = 0i64;
    let mut updated = 0i64;
    let mut skipped = 0i64;
    let regions = cfg
        .get("regions")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![json!(225)]);
    let region = py_str(&regions[0]);
    for item in &rows {
        let phrase = item["query"].as_str().unwrap_or_default();
        let values = item["values"].as_object().cloned().unwrap_or_default();
        let is_new = store.upsert(phrase, None, 0, Some("import"), &region)?;
        added += i64::from(is_new);
        let current = store
            .q1("SELECT * FROM phrases WHERE norm=?", &[json!(phrase)])?
            .unwrap_or_default();
        if !is_new && merge == "add" {
            skipped += 1;
            if current["status"].as_str() == Some("new")
                && has_observed_demand(
                    values.get("impr").and_then(Value::as_i64),
                    values.get("pos_y").and_then(Value::as_i64),
                    values.get("pos_g").and_then(Value::as_i64),
                )
            {
                store.set_status(
                    phrase,
                    "kept",
                    Some("observed search demand"),
                    Some("import"),
                )?;
            }
            continue;
        }
        let mut changed: Vec<(String, Value)> = Vec::new();
        if merge == "update" || merge == "replace" {
            for (k, v) in &values {
                changed.push((k.clone(), v.clone()));
            }
        } else {
            for (k, v) in &values {
                if current.get(k).is_none_or(|cv| cv.is_null()) {
                    changed.push((k.clone(), v.clone()));
                }
            }
        }
        if !changed.is_empty() {
            store.set_fields(phrase, &changed)?;
            updated += i64::from(!is_new);
        }
        if has_observed_demand(
            values.get("impr").and_then(Value::as_i64),
            values.get("pos_y").and_then(Value::as_i64),
            values.get("pos_g").and_then(Value::as_i64),
        ) {
            store.set_status(
                phrase,
                "kept",
                Some("observed search demand"),
                Some("import"),
            )?;
        }
    }
    let before_json = if merge == "replace" {
        json!(crate::pyfmt::dumps(
            &json!({"merge": "replace", "rows": preview["replacement_rows"]})
        ))
    } else {
        Value::Null
    };
    store.exec(
        "INSERT INTO semantic_audit(at,action,entity,before_json,after_json,evidence_json) \
         VALUES(?,?,?,?,?,?)",
        &[
            json!(zulu_seconds()),
            json!("import"),
            json!("phrases"),
            before_json,
            json!(crate::pyfmt::dumps(
                &json!({"merge": merge, "rows": added + updated})
            )),
            json!(crate::pyfmt::dumps(
                &json!({"file_type": preview["file_type"], "sheet": sheet})
            )),
        ],
    )?;
    store.commit()?;
    println!(
        "import: rows={} added={added} updated={updated} skipped_existing={skipped}",
        preview["unique_queries"]
    );
    let mut out = preview.as_object().cloned().unwrap_or_default();
    out.insert("added".into(), json!(added));
    out.insert("updated".into(), json!(updated));
    out.insert("skipped_existing".into(), json!(skipped));
    Ok(Value::Object(out))
}
