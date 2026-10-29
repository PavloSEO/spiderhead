use super::*;

pub(super) fn sort_key_cmp(left: &(bool, Value), right: &(bool, Value)) -> std::cmp::Ordering {
    // The caller only compares the key part; `missing` flags partition before sorting.
    match (&left.1, &right.1) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&b.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Number(_), _) => std::cmp::Ordering::Less,
        (_, Value::Number(_)) => std::cmp::Ordering::Greater,
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

/// `(result, selected_rows)` from `_apply_view_details`.
pub struct AppliedView {
    /// The public view result page.
    pub result: Value,
    /// The raw selected rows annotated with `__view_*` fields.
    pub selected_rows: Vec<Value>,
}

/// `_apply_view_details`: filter, sort, project and page one audit's findings.
pub fn apply_view_details(
    directory: &Path,
    view_name: &str,
    document: &Value,
    offset: i64,
) -> ProjectsResult<AppliedView> {
    if !document.is_object() {
        return Err(value("audit must be a JSON object"));
    }
    if !(0..=MAX_OFFSET).contains(&offset) {
        return Err(value(format!("offset must be within 0..{MAX_OFFSET}")));
    }
    let stored = show_view(directory, view_name)?;
    let view = &stored["view"];
    let definition = &view["definition"];
    let rows_key = if document.get("schema").is_some() || document.get("findings").is_some() {
        if document.get("schema").and_then(Value::as_str) != Some(SITE_AUDIT_SCHEMA)
            || !document.get("findings").is_some_and(Value::is_array)
        {
            return Err(value(
                "site audit must use the supported schema and findings list",
            ));
        }
        "findings"
    } else if document.get("schema_version").is_some() || document.get("issues").is_some() {
        if document.get("schema_version").and_then(Value::as_str) != Some("2.0")
            || !document.get("run").is_some_and(Value::is_object)
            || !document.get("summary").is_some_and(Value::is_object)
            || !document.get("issues").is_some_and(Value::is_array)
            || !document.get("pages").is_some_and(Value::is_array)
        {
            return Err(value(
                "SF audit must use schema_version 2.0 and the supported issue/page lists",
            ));
        }
        "issues"
    } else {
        return Err(value(
            "audit must use the supported site-audit or SF audit schema",
        ));
    };
    let rows = document[rows_key].as_array().cloned().unwrap_or_default();
    if rows.iter().any(|row| !row.is_object()) {
        return Err(value("audit finding rows must be JSON objects"));
    }
    let filters = definition["filters"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let columns = definition["columns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let sort = &definition["sort"];
    let sort_field = sort["field"].as_str().unwrap_or_default();
    let segment_needed = filters.contains_key("segment")
        || columns.iter().any(|c| c.as_str() == Some("segment"))
        || sort_field == "segment";
    let segment_data = if segment_needed {
        assign_view_segments(document, &rows)?
    } else {
        None
    };
    if (filters.contains_key("segment") || sort_field == "segment") && segment_data.is_none() {
        let operation = if filters.contains_key("segment") {
            "filtering"
        } else {
            "sorting"
        };
        return Err(value(format!(
            "segment {operation} is unavailable: source audit has no segment definitions"
        )));
    }
    let (segment_for, known_segments) = segment_data
        .map(|(segments, known)| (Some(segments), known))
        .unwrap_or_else(|| (None, std::collections::BTreeSet::new()));
    if let Some(selected) = filters.get("segment").and_then(Value::as_array) {
        let filter_set: std::collections::BTreeSet<String> = selected
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !filter_set.is_subset(&known_segments) {
            let unknown: Vec<String> = filter_set.difference(&known_segments).cloned().collect();
            let rendered = format!(
                "[{}]",
                unknown
                    .iter()
                    .map(|item| format!("'{item}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Err(value(format!(
                "view names segments absent from this audit: {rendered}"
            )));
        }
    }
    let mut matching: Vec<(Value, usize, Option<String>)> = Vec::new();
    let mut missing_filter_counts: Map<String, Value> = Map::new();
    for field_name in filters.keys() {
        missing_filter_counts.insert(field_name.clone(), json!(0));
    }
    for (ordinal, row) in rows.iter().enumerate() {
        let segment = segment_for
            .as_ref()
            .and_then(|segments| segments.get(ordinal).cloned());
        let mut keep = true;
        for (filter_field, selected) in &filters {
            let actual = if filter_field == "segment" {
                segment.clone().map(Value::String).unwrap_or(Value::Null)
            } else {
                field(row, filter_field)
            };
            if actual.is_null() || actual.as_str().is_some_and(str::is_empty) {
                if let Some(count) = missing_filter_counts.get_mut(filter_field) {
                    *count = json!(count.as_i64().unwrap_or(0) + 1);
                }
                keep = false;
            } else {
                let actual_text = crate::bindings::py_str(&actual);
                let selected_list = selected.as_array().cloned().unwrap_or_default();
                if !selected_list
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|item| item == actual_text)
                {
                    keep = false;
                }
            }
        }
        if keep {
            matching.push((row.clone(), ordinal, segment));
        }
    }
    let field_name = sort_field.to_string();
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for item in matching.into_iter() {
        let sort_value = if field_name == "segment" {
            item.2.clone().map(Value::String).unwrap_or(Value::Null)
        } else {
            field(&item.0, &field_name)
        };
        let (is_missing, _key) = sort_key(&sort_value, &field_name);
        if is_missing {
            missing.push(item);
        } else {
            present.push(item);
        }
    }
    let descending = sort["direction"].as_str() == Some("desc");
    present.sort_by(|a, b| {
        let av = if field_name == "segment" {
            a.2.clone().map(Value::String).unwrap_or(Value::Null)
        } else {
            field(&a.0, &field_name)
        };
        let bv = if field_name == "segment" {
            b.2.clone().map(Value::String).unwrap_or(Value::Null)
        } else {
            field(&b.0, &field_name)
        };
        let ordering = sort_key_cmp(&sort_key(&av, &field_name), &sort_key(&bv, &field_name));
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let matching: Vec<(Value, usize, Option<String>)> =
        present.into_iter().chain(missing).collect();
    let mut source_identity = document
        .get("run")
        .filter(|r| !crate::schedule::falsy(r))
        .and_then(|r| r.get("scan_uuid"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if source_identity.is_none() {
        let mut digest = Sha256::new();
        for source_row in &rows {
            digest.update(crate::pyjson::dumps_compact(source_row).as_bytes());
            digest.update(b"\0");
        }
        source_identity = Some(format!("{:x}", digest.finalize()));
    }
    let source_identity = source_identity.unwrap_or_default();
    let page_size = definition["page_size"].as_i64().unwrap_or(100);
    let start = (offset as usize).min(matching.len());
    let end = (start + page_size as usize).min(matching.len());
    let selected = &matching[start..end];
    let mut missing_projection_counts: Map<String, Value> = Map::new();
    for column in &columns {
        missing_projection_counts.insert(column.as_str().unwrap_or_default().to_string(), json!(0));
    }
    for (row, _ordinal, segment) in &matching {
        for column in &columns {
            let column_name = column.as_str().unwrap_or_default();
            let entry = if column_name == "segment" {
                segment.clone().map(Value::String).unwrap_or(Value::Null)
            } else {
                field(row, column_name)
            };
            if entry.is_null() || entry.as_str().is_some_and(str::is_empty) {
                if let Some(count) = missing_projection_counts.get_mut(column_name) {
                    *count = json!(count.as_i64().unwrap_or(0) + 1);
                }
            }
        }
    }
    let mut output_rows = Vec::new();
    for (row, ordinal, segment) in selected {
        let mut values = Map::new();
        let mut missing_fields = Vec::new();
        for column in &columns {
            let column_name = column.as_str().unwrap_or_default();
            let entry = if column_name == "segment" {
                segment.clone().map(Value::String).unwrap_or(Value::Null)
            } else {
                field(row, column_name)
            };
            if entry.is_null() || entry.as_str().is_some_and(str::is_empty) {
                missing_fields.push(Value::String(column_name.to_string()));
            }
            values.insert(column_name.to_string(), entry);
        }
        let identity_payload = crate::pyjson::dumps_compact(row);
        let finding_id = format!(
            "{:x}",
            Sha256::digest(format!("{source_identity}\0{ordinal}\0{identity_payload}").as_bytes())
        );
        output_rows.push(json!({
            "finding_id": finding_id,
            "fields": Value::Object(values),
            "missing_fields": missing_fields,
        }));
    }
    let total = matching.len() as i64;
    let has_more = (offset + selected.len() as i64) < total;
    let filtered_counts: Map<String, Value> = missing_filter_counts
        .iter()
        .filter(|(_, v)| v.as_i64().is_some_and(|n| n != 0))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let filtered_projection: Map<String, Value> = missing_projection_counts
        .iter()
        .filter(|(_, v)| v.as_i64().is_some_and(|n| n != 0))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let state = if missing_filter_counts
        .values()
        .any(|v| v.as_i64().is_some_and(|n| n != 0))
        || missing_projection_counts
            .values()
            .any(|v| v.as_i64().is_some_and(|n| n != 0))
    {
        "partial"
    } else {
        "measured"
    };
    let result = json!({
        "ok": true,
        "state": state,
        "view": {
            "id": view["id"],
            "name": view["name"],
            "schema_version": view["schema_version"],
            "revision": view["revision"],
            "config_revision": stored["config_revision"],
        },
        "source": {
            "identity": source_identity,
            "schema_version": document
                .get("schema_version")
                .filter(|v| !crate::schedule::falsy(v))
                .or_else(|| document.get("schema"))
                .cloned()
                .unwrap_or(Value::Null),
            "rows_key": rows_key,
            "finding_exclusion_policy": document
                .get("run")
                .filter(|r| !crate::schedule::falsy(r))
                .and_then(|r| r.get("finding_exclusion_policy"))
                .cloned()
                .unwrap_or(Value::Null),
            "finding_exclusions": document
                .get("summary")
                .filter(|s| !crate::schedule::falsy(s))
                .and_then(|s| s.get("finding_exclusions"))
                .cloned()
                .unwrap_or(Value::Null),
        },
        "counts": {
            "source": rows.len() as i64,
            "matched": total,
            "returned": selected.len() as i64,
            "filtered": rows.len() as i64 - total,
            "missing_filter_fields": filtered_counts,
            "missing_projection_fields": filtered_projection,
        },
        "pagination": {
            "offset": offset,
            "page_size": page_size,
            "has_more": has_more,
            "truncated": has_more,
            "next_offset": if has_more { json!(offset + selected.len() as i64) } else { Value::Null },
        },
        "columns": columns,
        "sort": sort.clone(),
        "items": output_rows,
    });
    let mut selected_rows = Vec::new();
    for (index, (row, _ordinal, segment)) in selected.iter().enumerate() {
        let mut copied = row.clone();
        if segment_for.is_some() {
            copied["__view_segment"] = Value::String(segment.clone().unwrap_or_default());
        }
        copied["__view_finding_id"] = output_rows_get(&result, index, "finding_id");
        copied["__view_missing_fields"] = output_rows_get(&result, index, "missing_fields");
        selected_rows.push(copied);
    }
    Ok(AppliedView {
        result,
        selected_rows,
    })
}

pub(super) fn output_rows_get(result: &Value, index: usize, field: &str) -> Value {
    result["items"]
        .as_array()
        .and_then(|items| items.get(index))
        .and_then(|item| item.get(field).cloned())
        .unwrap_or(Value::Null)
}

/// `apply_view_to_audit`: filter, sort, project and page findings in place.
pub fn apply_view_to_audit(
    directory: &Path,
    name: &str,
    document: &Value,
    offset: i64,
) -> ProjectsResult<Value> {
    Ok(apply_view_details(directory, name, document, offset)?.result)
}
