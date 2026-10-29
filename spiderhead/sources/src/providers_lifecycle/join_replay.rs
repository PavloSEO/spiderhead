//! Split out of `providers_lifecycle`.

use super::*;

/// `provider_join(crawl_pages, evidence_rows, *, url_field, url_column,
/// review_external_only, adjustments)`.
pub fn provider_join(
    crawl_pages: &[Value],
    evidence_rows: &[Value],
    url_field: &str,
    url_column: &str,
    review_external_only: bool,
    adjustments: Option<&[Value]>,
) -> Result<Value, CfError> {
    let joined = external_join::join_external_data(
        crawl_pages,
        evidence_rows,
        url_field,
        url_column,
        &external_join::default_key_fn,
    )?;
    let candidates = if review_external_only {
        external_join::orphan_urls(&joined, url_column, None)?
    } else {
        Vec::new()
    };
    let mut applied: Vec<Value> = Vec::new();
    for rule in adjustments.unwrap_or(&[]) {
        let required = ["source_fields", "period", "coverage", "adjustment"];
        let valid_shape = rule
            .as_object()
            .map(|m| required.iter().all(|k| m.contains_key(*k)))
            .unwrap_or(false);
        if !valid_shape {
            return Err(CfError::Value(
                "priority adjustment requires source fields, period, coverage, and adjustment"
                    .to_owned(),
            ));
        }
        let fields = &rule["source_fields"];
        let valid_fields = fields
            .as_array()
            .map(|a| !a.is_empty() && a.iter().all(|n| n.as_str().is_some_and(|s| !s.is_empty())))
            .unwrap_or(false);
        if !valid_fields {
            return Err(CfError::Value(
                "priority adjustment requires named source fields".to_owned(),
            ));
        }
        let coverage = rule["coverage"].as_str().unwrap_or_default();
        if !matches!(
            coverage,
            "complete" | "partial" | "sampled" | "truncated" | "unmatched" | "privacy_thresholded"
        ) {
            return Err(CfError::Value(
                "priority adjustment has unsupported coverage".to_owned(),
            ));
        }
        if coverage != "complete" {
            applied.push(json!({
                "applied": false,
                "reason": "prioritization evidence unavailable",
                "rule": rule,
            }));
            continue;
        }
        let source_fields = fields.as_array().cloned().unwrap_or_default();
        let matches: Vec<&Value> = joined["joined"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        source_fields.iter().all(|field| {
                            let name = field.as_str().unwrap_or_default();
                            let external = &entry["external"];
                            py_contains(external, name)
                                && !external.get(name).is_none_or(|v| v.is_null())
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if matches.is_empty() {
            applied.push(json!({
                "applied": false,
                "reason": "no matched source-backed rows",
                "rule": rule,
            }));
            continue;
        }
        for entry in matches {
            applied.push(json!({
                "applied": true,
                "url": entry["url"],
                "adjustment": rule["adjustment"],
                "source_fields": rule["source_fields"],
                "period": rule["period"],
                "technical_severity": entry["page"].get("severity").cloned().unwrap_or(Value::Null),
                "technical_severity_changed": false,
            }));
        }
    }
    Ok(json!({
        "format": "spiderhead.provider-join.v1",
        "join": joined,
        "list_crawl_candidates": candidates,
        "frontier_mutated": false,
        "priority_adjustments": applied,
    }))
}

/// `name in container` — dict keys, list equality, string substring.
pub(super) fn py_contains(container: &Value, name: &str) -> bool {
    match container {
        Value::Object(map) => map.contains_key(name),
        Value::Array(items) => items.iter().any(|v| v == &json!(name)),
        Value::String(text) => text.contains(name),
        _ => false,
    }
}

/// `provider_replay(input_path, evidence_file, out_dir, *, url_column,
/// review_external_only)` — offline join of saved evidence to a saved scan.
pub fn provider_replay(
    env: &ProviderEnv,
    input_path: &str,
    evidence_file: &str,
    out_dir: &str,
    url_column: &str,
    review_external_only: bool,
) -> Result<Value, CfError> {
    use spiderhead_core::filesystem::StatCompat;
    let source = std::path::PathBuf::from(evidence_file);
    let meta = source.symlink_metadata().ok();
    let invalid = meta
        .map(|m| {
            m.file_type().is_symlink()
                || !m.is_file()
                || !credentials::is_private_mode(m.mode())
                || m.len() > 16 * 1024 * 1024
        })
        .unwrap_or(true);
    if invalid {
        return Err(CfError::Value(
            "evidence_file must be a private bounded regular JSON file".to_owned(),
        ));
    }
    let stored_bytes =
        std::fs::read(&source).map_err(|e| CfError::Python("OSError", format!("[Errno 2] {e}")))?;
    let stored: Value = serde_json::from_slice(&stored_bytes)
        .map_err(|e| CfError::Python("JSONDecodeError", e.to_string()))?;
    let valid_envelope = stored
        .as_object()
        .map(|m| m.len() == 2 && m.contains_key("evidence") && m.contains_key("result"))
        .unwrap_or(false);
    if !valid_envelope {
        return Err(CfError::Value(
            "expected a saved provider collection envelope".to_owned(),
        ));
    }
    let evidence_saved = &stored["evidence"];
    let result = &stored["result"];
    if !evidence_saved.is_object()
        || evidence_saved.get("format") != Some(&json!(EVIDENCE_FORMAT))
        || !result.is_object()
    {
        return Err(CfError::Value("invalid saved provider evidence".to_owned()));
    }
    let mut rows = result.get("rows").cloned().unwrap_or(Value::Null);
    let rows_ok = rows
        .as_array()
        .map(|r| r.len() <= 100_000 && r.iter().all(Value::is_object))
        .unwrap_or(false);
    if !rows_ok {
        return Err(CfError::Value(
            "saved provider operation does not contain bounded joinable rows".to_owned(),
        ));
    }
    let mut url_column = url_column.to_owned();
    if evidence_saved.get("provider") == Some(&json!("gsc")) {
        let dimensions = result
            .get("dimensions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(index) = dimensions.iter().position(|d| d == &json!("page")) {
            rows = Value::Array(
                rows.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|row| {
                        let mut row = row.clone();
                        let url = row
                            .get("keys")
                            .and_then(Value::as_array)
                            .and_then(|keys| keys.get(index))
                            .cloned()
                            .unwrap_or(Value::Null);
                        row["url"] = url;
                        row
                    })
                    .collect(),
            );
            url_column = "url".to_owned();
        }
    }
    let (page_count, pages, scan_uuid) = (env.open_scan)(input_path)?;
    if page_count > 100_000 {
        return Err(CfError::Value(
            "saved provider join exceeds the page bound".to_owned(),
        ));
    }
    let row_vec = rows.as_array().cloned().unwrap_or_default();
    let mut joined = provider_join(
        &pages,
        &row_vec,
        "url",
        &url_column,
        review_external_only,
        None,
    )?;
    use sha2::Digest;
    joined["source"] = json!({
        "scan_uuid": scan_uuid,
        "provider_evidence_sha256": format!("{:x}", sha2::Sha256::digest(&stored_bytes)),
        "provider": evidence_saved.get("provider").cloned().unwrap_or(Value::Null),
        "period": evidence_saved.get("period").cloned().unwrap_or(Value::Null),
        "status": evidence_saved.get("status").cloned().unwrap_or(Value::Null),
        "sampling": evidence_saved.get("sampling").cloned().unwrap_or(Value::Null),
    });
    let reference = save_local_artifact(out_dir, &joined)?;
    Ok(json!({
        "ok": true,
        "format": "spiderhead.provider-replay.v1",
        "counts": joined["join"]["summary"],
        "artifact_reference": reference,
        "provider_status": evidence_saved.get("status").cloned().unwrap_or(Value::Null),
        "frontier_mutated": false,
        "priority_applied": false,
    }))
}
