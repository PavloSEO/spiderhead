use super::*;

/// `_validate_document`: validate the complete `coverage.json` across all four formats.
pub fn validate_document(document: &Value, project: &Value) -> ProjectsResult<Value> {
    let mut document = document.clone();
    let format = document["format"].as_str().map(str::to_owned);
    let expected_keys: BTreeSet<&str> = match format.as_deref() {
        Some(FORMAT) => ["format", "version", "project_uuid", "revision", "items"]
            .into_iter()
            .collect(),
        Some(FORMAT_V2) | Some(FORMAT_V3) => [
            "format",
            "version",
            "project_uuid",
            "revision",
            "items",
            "priority_policy",
        ]
        .into_iter()
        .collect(),
        Some(FORMAT_V4) => [
            "format",
            "version",
            "project_uuid",
            "revision",
            "items",
            "priority_policy",
            "plans",
        ]
        .into_iter()
        .collect(),
        _ => return Err(crate::error::value("unsupported coverage document shape")),
    };
    if document["version"].as_i64().is_none() || document["version"].is_boolean() {
        return Err(crate::error::value("unsupported coverage document shape"));
    }
    if matches!(format.as_deref(), Some(FORMAT_V3) | Some(FORMAT_V4)) {
        let extra: BTreeSet<&str> = document
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|k| !expected_keys.contains(k))
            .collect();
        let allowed_extra: BTreeSet<&str> = ["plan", "plans"].into_iter().collect();
        if !extra.is_subset(&allowed_extra) {
            return Err(crate::error::value("unsupported coverage document shape"));
        }
        if let Some(plan) = document.as_object().unwrap().get("plan").cloned() {
            document.as_object_mut().unwrap().remove("plan");
            document["plans"] = Value::Array(vec![plan]);
        }
    } else if document
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect::<BTreeSet<_>>())
        .unwrap_or_default()
        != expected_keys
    {
        return Err(crate::error::value("unsupported coverage document shape"));
    }
    let expected_version = match format.as_deref() {
        Some(FORMAT) => 1,
        Some(FORMAT_V2) => 2,
        Some(FORMAT_V3) => 3,
        _ => 4,
    };
    if document["version"].as_i64() != Some(expected_version) {
        return Err(crate::error::value("unsupported coverage version"));
    }
    if document["project_uuid"].as_str() != project["project_uuid"].as_str() {
        return Err(crate::error::value("coverage project UUID mismatch"));
    }
    match document["revision"].as_i64() {
        Some(v) if !document["revision"].is_boolean() && v >= 1 => {}
        _ => return Err(crate::error::value("invalid coverage revision/items")),
    }
    let items = document["items"]
        .as_object()
        .ok_or_else(|| crate::error::value("invalid coverage revision/items"))?;
    if items.len() > 10000 {
        return Err(crate::error::value("too many checklist items"));
    }
    let task_fields: BTreeSet<&str> = TASK_FIELDS.iter().copied().collect();
    for (item_id, item) in items {
        identifier(&json_from(item_id))?;
        let mut allowed: BTreeSet<&str> = ["definition", "definitions", "records"]
            .into_iter()
            .collect();
        if format.as_deref() == Some(FORMAT_V4) {
            allowed.insert("comments");
        }
        let object = item
            .as_object()
            .filter(|o| o.keys().map(String::as_str).collect::<BTreeSet<_>>() == allowed);
        let Some(object) = object else {
            return Err(crate::error::value("invalid checklist history"));
        };
        if !object["definition"].is_object()
            || object["definition"].get("id").and_then(Value::as_str) != Some(item_id)
        {
            return Err(crate::error::value("item identity mismatch"));
        }
        if !object["definitions"].is_array() || !object["records"].is_array() {
            return Err(crate::error::value("invalid checklist history lists"));
        }
        let definitions = object["definitions"].as_array().unwrap();
        let last = definitions.last().cloned().unwrap_or(Value::Null);
        if definitions.is_empty()
            || !last["definition"].is_object()
            || last["definition"] != object["definition"]
        {
            return Err(crate::error::value(
                "current definition is absent from history",
            ));
        }
        definition(&object["definition"], &Map::new(), true)?;
        if format.as_deref() == Some(FORMAT_V4) {
            let keys: BTreeSet<&str> = object["definition"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            if !task_fields.is_subset(&keys) {
                return Err(crate::error::value("v4 task metadata is incomplete"));
            }
            let comments = object["comments"].as_array().filter(|c| c.len() <= 10000);
            let Some(comments) = comments else {
                return Err(crate::error::value("invalid task comment history"));
            };
            for comment in comments {
                task_comment_shape(comment)?;
            }
        }
        if format.as_deref() == Some(FORMAT)
            && object["definition"]["priority_origin"].as_str() == Some("policy")
        {
            return Err(crate::error::value(
                "v1 coverage cannot contain policy priority origins",
            ));
        }
        let mut hashes: BTreeMap<String, Value> = BTreeMap::new();
        for version in definitions {
            let v = version.as_object().filter(|o| {
                o.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["observed_at", "definition"].into_iter().collect()
            });
            let Some(v) = v else {
                return Err(crate::error::value("invalid definition history entry"));
            };
            text(&v["observed_at"], "definition observation time", 128)?;
            let validated = definition(&v["definition"], &Map::new(), true)?;
            if validated["id"].as_str() != Some(item_id.as_str()) {
                return Err(crate::error::value("historical item identity mismatch"));
            }
            if format.as_deref() == Some(FORMAT)
                && validated["priority_origin"].as_str() == Some("policy")
            {
                return Err(crate::error::value(
                    "v1 coverage cannot contain policy priority origins",
                ));
            }
            hashes.insert(completion_hash(&validated)?, validated);
        }
        for record in object["records"].as_array().unwrap() {
            record_shape(record, &hashes)?;
        }
    }
    if matches!(format.as_deref(), Some(FORMAT_V2) | Some(FORMAT_V3)) {
        let policy = document["priority_policy"].as_object().filter(|p| {
            p.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["applications"].into_iter().collect()
                && p["applications"].is_array()
        });
        let Some(policy) = policy else {
            return Err(crate::error::value("invalid priority policy history"));
        };
        for application in policy["applications"].as_array().unwrap() {
            let object = application.as_object().filter(|a| {
                a.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["applied_at", "receipt"].into_iter().collect()
            });
            let Some(object) = object else {
                return Err(crate::error::value("invalid priority policy application"));
            };
            application_time(&object["applied_at"])?;
            let ids: BTreeSet<String> = items.keys().cloned().collect();
            priority_receipt(&object["receipt"], &ids)?;
        }
    }
    if matches!(format.as_deref(), Some(FORMAT_V3) | Some(FORMAT_V4)) {
        let plans = document["plans"].as_array().filter(|p| {
            !(format.as_deref() == Some(FORMAT_V3) && p.is_empty()) && p.len() <= 10000
        });
        let Some(plans) = plans else {
            return Err(crate::error::value("invalid audit scope plan history"));
        };
        let mut revision = 0;
        for entry in plans {
            stored_plan(
                entry,
                project["site"]["target"].as_str().unwrap_or_default(),
            )?;
            let current = entry["revision"].as_i64().unwrap_or(0);
            if current <= revision {
                return Err(crate::error::value(
                    "audit scope plan revisions must increase",
                ));
            }
            revision = current;
        }
    }
    dependencies(items)?;
    Ok(document)
}

pub(super) fn json_from(text: &str) -> Value {
    Value::String(text.to_owned())
}

/// `validate_document` over a mutable document (identity preserved for callers).
pub fn validate_in_place(document: &mut Value, project: &Value) -> ProjectsResult<()> {
    *document = validate_document(document, project)?;
    Ok(())
}

#[allow(dead_code)]
pub(super) type _Unused = HashMap<String, Value>;
