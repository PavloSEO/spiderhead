//! Scalar-storage and import-metadata validation of scan.v1 artifacts.

use super::*;

// slice: storage-__init__
/// Every column holds its declared storage class; no non-finite or oversized text.
pub fn validate_scalar_storage(con: &Connection) -> Result<(), StoreError> {
    for (table, columns) in &expected()?.columns {
        let mut conditions = Vec::new();
        for column in columns {
            let value = format!("\"{}\"", column.name);
            let mut types = match column.kind.as_str() {
                "INTEGER" => "'integer'".to_owned(),
                "REAL" => "'integer','real'".to_owned(),
                "TEXT" => "'text'".to_owned(),
                "BLOB" => "'blob'".to_owned(),
                other => return Err(bad(format!("{table}: unsupported column type {other}"))),
            };
            if !column.required && !column.primary {
                types.push_str(",'null'");
            }
            conditions.push(format!("typeof({value}) NOT IN ({types})"));
            if column.kind == "REAL" {
                conditions.push(format!("abs({value}) > 1.7976931348623157e308"));
            }
            if column.kind == "TEXT" {
                let limit = if column.name == "document_json" {
                    MAX_JSON_BYTES
                } else {
                    MAX_RECORD_BYTES
                };
                conditions.push(format!("length(CAST({value} AS BLOB)) > {limit}"));
            }
        }
        let sql = format!(
            "SELECT 1 FROM \"{table}\" WHERE {} LIMIT 1",
            conditions.join(" OR ")
        );
        if con.prepare(&sql)?.exists([])? {
            return Err(bad(format!(
                "{table}: invalid scalar type, non-finite number or oversized value"
            )));
        }
    }
    Ok(())
}

/// `YYYY-MM-DDTHH:MM[:SS[.ffffff]]` with an explicit zero UTC offset (`Z` or `+00:00`).
pub(super) fn is_utc_timestamp(text: &str) -> bool {
    let Some(body) = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))
    else {
        return false;
    };
    let (date, time) = match body.split_once(['T', ' ']) {
        Some(parts) => parts,
        None => return false,
    };
    let d: Vec<&str> = date.split('-').collect();
    if d.len() != 3 || d[0].len() != 4 || d[1].len() != 2 || d[2].len() != 2 {
        return false;
    }
    let (Ok(y), Ok(m), Ok(day)) = (
        d[0].parse::<i64>(),
        d[1].parse::<i64>(),
        d[2].parse::<i64>(),
    ) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if !(1..=days).contains(&day) {
        return false;
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((c, f)) => (c, Some(f)),
        None => (time, None),
    };
    if let Some(f) = fraction {
        if f.is_empty() || f.len() > 6 || !f.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let parts: Vec<&str> = clock.split(':').collect();
    if !(parts.len() == 2 || parts.len() == 3) || parts.iter().any(|p| p.len() != 2) {
        return false;
    }
    let nums: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    nums.len() == parts.len() && nums[0] < 24 && nums[1] < 60 && nums.get(2).is_none_or(|s| *s < 60)
}

/// Accept a UUID in any form `uuid.UUID` takes (hyphenated, simple, braced, urn).
pub(crate) fn is_uuid(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok()
}

// slice: storage-__init__
/// Cross-check the header row, the saved audit and the legacy import provenance.
pub fn validate_import_metadata(
    con: &Connection,
    scan: &Map<String, Value>,
    audit: &Value,
) -> Result<(), StoreError> {
    if (
        field_str(scan, "lifecycle")?,
        field_str(scan, "finish_reason")?,
        field_str(scan, "evidence_version")?,
        field_int(scan, "corpus_partial")?,
    ) != ("finished", "legacy_import", "crawl.v1", 1)
    {
        return Err(bad(
            "unsupported point-A lifecycle, evidence version or corpus completeness",
        ));
    }
    if !is_uuid(field_str(scan, "scan_uuid")?)
        || !is_utc_timestamp(field_str(scan, "created_at")?)
        || !is_utc_timestamp(field_str(scan, "finished_at")?)
    {
        return Err(bad("invalid scan UUID or UTC timestamp"));
    }
    let run_source = audit
        .get("run")
        .and_then(|r| r.get("source"))
        .cloned()
        .unwrap_or(Value::Null);
    if !scan.get("parent_scan_uuid").is_none_or(Value::is_null)
        || scan.get("start_url").cloned().unwrap_or(Value::Null) != run_source
    {
        return Err(bad(
            "legacy scan lineage or start URL disagrees with its source audit",
        ));
    }
    let versions = loads(
        field_str(scan, "runtime_versions_json")?,
        "runtime_versions_json",
    )?;
    let version_ok = versions.as_object().is_some_and(|o| {
        o.len() == 5
            && ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"]
                .iter()
                .all(|k| {
                    o.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty())
                })
    });
    if !version_ok {
        return Err(bad("invalid runtime version provenance"));
    }
    let limitations = loads(field_str(scan, "limitations_json")?, "limitations_json")?;
    let Some(limit_items) = limitations.as_array() else {
        return Err(bad("limitations_json must be a string list"));
    };
    if !limit_items.iter().all(Value::is_string) {
        return Err(bad("limitations_json must be a string list"));
    }
    let limit_notes: Vec<String> = limit_items
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let retention = loads(field_str(scan, "retention_json")?, "retention_json")?;
    let expected_retention = serde_json::json!({
        "policy_version": "scan_retention.v1",
        "body_mode": "off",
        "max_body_bytes": 0,
        "max_body_store_bytes": 0,
        "min_free_bytes": 0,
        "history_warning_bytes": 0,
        "automatic_delete": false,
    });
    if dump(&retention) != dump(&expected_retention) {
        return Err(bad("unsupported point-A body retention policy"));
    }
    if has_negative_page_counts(con)? {
        return Err(bad("invalid negative PageRecord count"));
    }
    if con
        .prepare(
            "SELECT 1 FROM pages WHERE content_frames_same_origin > content_frames \
             OR body_unavailable NOT IN \
             ('','oversized','excluded_by_media_type','not_included_by_media_type','media_type_unavailable') LIMIT 1",
        )?
        .exists([])?
    {
        return Err(bad("invalid frame counts or body_unavailable state"));
    }
    let (count, low, high): (i64, Option<i64>, Option<i64>) = con.query_row(
        "SELECT COUNT(*), MIN(page_ordinal), MAX(page_ordinal) FROM pages",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if count > 0 && (low != Some(0) || high != Some(count - 1)) {
        return Err(bad("page ordinals are not a contiguous imported sequence"));
    }
    if con
        .prepare(
            "SELECT 1 FROM links GROUP BY source_url_id \
             HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1",
        )?
        .exists([])?
    {
        return Err(bad(
            "link ordinals are not a contiguous per-source sequence",
        ));
    }
    let context = query_maps(
        con,
        "SELECT * FROM context_items WHERE kind!='legacy_render_blocking'",
    )?;
    let [item] = context.as_slice() else {
        return Err(bad("point-A import requires legacy_import_provenance only"));
    };
    if (
        field_str(item, "kind")?,
        field_str(item, "item_key")?,
        field_str(item, "payload_version")?,
    ) != ("legacy_import_provenance", "run", "scan_context.v1")
    {
        return Err(bad("point-A import requires legacy_import_provenance only"));
    }
    if field_str(item, "reason")? != "resume unavailable" {
        return Err(bad("point-A import must report resume unavailable"));
    }
    let provenance = loads(field_str(item, "payload_json")?, "legacy_import_provenance")?;
    let Some(po) = provenance.as_object() else {
        return Err(bad("invalid legacy import provenance"));
    };
    let prov_keys: BTreeSet<&str> = po.keys().map(String::as_str).collect();
    let expected_keys: BTreeSet<&str> = [
        "source_format",
        "inputs",
        "recovered_truncated_final_line",
        "resume_eligible",
    ]
    .into_iter()
    .collect();
    if prov_keys != expected_keys
        || po["source_format"] != "legacy_directory.v1"
        || po["resume_eligible"] != Value::Bool(false)
        || !po["recovered_truncated_final_line"].is_boolean()
    {
        return Err(bad("invalid legacy import provenance"));
    }
    let inputs = po["inputs"]
        .as_array()
        .filter(|items| items.len() == 3)
        .ok_or_else(|| bad("invalid legacy input manifest"))?;
    if !inputs.iter().all(|x| {
        x.as_object().is_some_and(|o| {
            o.len() == 3
                && o.contains_key("name")
                && o.contains_key("sha256")
                && o.contains_key("bytes")
        })
    }) {
        return Err(bad("invalid legacy input manifest"));
    }
    let names: Vec<&str> = inputs.iter().filter_map(|x| x["name"].as_str()).collect();
    if names != ["pages.jsonl", "links.jsonl", "audit.json"] {
        return Err(bad(
            "legacy input manifest must identify pages, links and audit exactly once",
        ));
    }
    for item in inputs {
        let sha_ok = item["sha256"].as_str().is_some_and(|h| {
            h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        let bytes_ok = item["bytes"].as_i64().is_some_and(|b| b >= 0);
        if !sha_ok || !bytes_ok {
            return Err(bad("invalid legacy input hash or byte count"));
        }
    }
    let (original_document, original_sha): (String, String) =
        con.query_row("SELECT document_json, sha256 FROM audit", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let last = &inputs[2];
    if last["sha256"].as_str() != Some(original_sha.as_str())
        || last["bytes"].as_i64() != Some(original_document.len() as i64)
    {
        return Err(bad(
            "legacy audit input manifest does not match the retained document",
        ));
    }
    for item in query_maps(
        con,
        "SELECT * FROM context_items WHERE kind='legacy_render_blocking'",
    )? {
        validate_legacy_render_blocking_context(con, &item)?;
    }
    let recovered_notes = limit_notes
        .iter()
        .filter(|n| n.contains("recovered a truncated final line"))
        .count();
    let recovered_flag = recovered_notes > 0;
    if recovered_notes > 1
        || recovered_flag
            != po["recovered_truncated_final_line"]
                .as_bool()
                .unwrap_or(false)
    {
        return Err(bad(
            "truncated recovery provenance disagrees with limitations",
        ));
    }
    let partial = audit
        .get("run")
        .and_then(|r| r.get("crawl_partial"))
        .is_some_and(truthy)
        || recovered_flag;
    let completeness = field_str(item, "completeness")?;
    if truthy(&scan.get("crawl_partial").cloned().unwrap_or(Value::Null)) != partial
        || completeness != if partial { "partial" } else { "complete" }
    {
        return Err(bad("partialness and import provenance disagree"));
    }
    let capabilities = loads(field_str(scan, "capabilities_json")?, "capabilities_json")?;
    let missing = legacy_fields_missing(con)?;
    let missing_notes: Vec<&String> = limit_notes
        .iter()
        .filter(|n| n.starts_with("legacy page fields unavailable:"))
        .collect();
    let expected_note = format!("legacy page fields unavailable: {}", missing.join(", "));
    let expected_notes: Vec<String> = if missing.is_empty() {
        vec![]
    } else {
        vec![expected_note]
    };
    if missing_notes.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        != expected_notes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    {
        return Err(bad(
            "legacy field availability disagrees with recorded limitations",
        ));
    }
    for name in ["pages", "links"] {
        let unavailable = partial || (name == "pages" && !missing.is_empty());
        let state = capabilities
            .get(name)
            .and_then(|c| c.get("state"))
            .and_then(Value::as_str);
        let wanted = if unavailable { "partial" } else { "complete" };
        if state != Some(wanted) {
            return Err(bad("imported page/link capability completeness disagrees"));
        }
    }
    Ok(())
}
