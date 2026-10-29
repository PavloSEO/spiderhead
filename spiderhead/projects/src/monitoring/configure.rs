use super::*;

/// `configure`: store the bounded policy; nothing is scheduled by this call.
pub fn configure(
    directory: &Path,
    policy: &Value,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    let (root, mut document) = monitor_load(directory)?;
    if document["revision"] != json!(expected_revision) {
        return Err(value("monitor revision conflict"));
    }
    if document["runner"].get("state").and_then(Value::as_str) == Some("running") {
        return Err(value(
            "cancel the active local runner before changing its policy",
        ));
    }
    document["policy"] = monitor_policy(policy)?;
    document["revision"] = json!(expected_revision + 1);
    write_document(&root, NAME, &document, Some(expected_revision))?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "policy": document["policy"],
        "state": "configured_not_scheduled",
        "due": due(&document)?,
    }))
}

pub(super) fn change_key(change: &Value) -> String {
    dumps_compact(change)
}

pub(super) fn threshold(policy: &Value, change: &Value) -> bool {
    let severity = change
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("notice");
    let have = SEVERITIES.iter().position(|s| *s == severity).unwrap_or(0);
    let need = SEVERITIES
        .iter()
        .position(|s| Some(*s) == policy["severity_threshold"].as_str())
        .unwrap_or(0);
    have >= need
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn is_hex64(text: &Value) -> bool {
    text.as_str()
        .map(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .unwrap_or(false)
}

/// `_source`: validate caller-declared provenance against retained bytes.
pub(super) fn source(item: &Value, root: &Path) -> ProjectsResult<Option<Value>> {
    if item.is_null() {
        return Ok(None);
    }
    let map = match item.as_object() {
        Some(map)
            if map.len() == 5
                && [
                    "body_ref",
                    "body_sha256",
                    "validation_ref",
                    "validation_sha256",
                    "links_sha256",
                ]
                .iter()
                .all(|key| map.contains_key(*key)) =>
        {
            map
        }
        _ => return Err(value("monitor source provenance is invalid")),
    };
    for (ref_key, hash_key) in [
        ("body_ref", "body_sha256"),
        ("validation_ref", "validation_sha256"),
    ] {
        let reference = &map[ref_key];
        let digest_expected = &map[hash_key];
        if reference.is_null() && digest_expected.is_null() && ref_key == "body_ref" {
            continue;
        }
        let rel = reference.as_str();
        let safe = rel
            .map(|text| {
                text.starts_with("reports/monitor/")
                    && !Path::new(text).components().any(|c| c.as_os_str() == "..")
            })
            .unwrap_or(false);
        if !safe || !is_hex64(digest_expected) {
            return Err(value("monitor source provenance is invalid"));
        }
        let path = artifact_path(root, reference)?;
        let policy = project_policy(root, None, false, None, None)?;
        let budget = &policy["policy"]["evidence_hash"];
        let actual = digest(
            &path,
            budget
                .get("max_seconds")
                .and_then(Value::as_f64)
                .map(|secs| monotonic() + secs),
            budget.get("max_bytes").and_then(Value::as_u64),
        )?;
        if &json!(actual) != digest_expected {
            return Err(value(
                "monitor source provenance does not match retained bytes",
            ));
        }
    }
    if !is_hex64(&map["links_sha256"]) {
        return Err(value("monitor source provenance is invalid"));
    }
    Ok(Some(item.clone()))
}

pub(super) const OBSERVATION_KEYS: [&str; 9] = [
    "url",
    "changes",
    "qualifier",
    "measurement",
    "request_count",
    "render_request_count",
    "cache_state",
    "failure_reason",
    "source",
];

/// `_observation`: normalize one caller-imported observation.
pub(super) fn observation(
    item: &Value,
    policy: &Value,
    root: &Path,
    now: &dyn Fn() -> String,
) -> ProjectsResult<Value> {
    let map = match item.as_object() {
        Some(map)
            if map
                .keys()
                .all(|key| OBSERVATION_KEYS.contains(&key.as_str())) =>
        {
            map
        }
        _ => return Err(value("invalid scoped monitor observation")),
    };
    let url = map.get("url").cloned().unwrap_or(Value::Null);
    let in_urls = policy["urls"]
        .as_array()
        .map(|urls| urls.iter().any(|u| py_eq(u, &url)))
        .unwrap_or(false);
    if !in_urls || !matches!(item.get("changes"), Some(Value::Array(_))) {
        return Err(value("invalid scoped monitor observation"));
    }
    let qualifier = item
        .get("qualifier")
        .and_then(Value::as_str)
        .unwrap_or("fresh")
        .to_string();
    if !QUALIFIERS.contains(&qualifier.as_str()) {
        return Err(value("monitor observation qualifier is invalid"));
    }
    let mut cache_state = item.get("cache_state").cloned().unwrap_or(Value::Null);
    if cache_state.is_null() {
        cache_state = json!(cache_states(&qualifier)[0]);
    }
    if !cache_states(&qualifier).contains(&cache_state.as_str().unwrap_or_default()) {
        return Err(value(
            "monitor observation qualifier and cache_state are inconsistent",
        ));
    }
    let request_count = item.get("request_count").cloned().unwrap_or(json!(1));
    let render_request_count = item
        .get("render_request_count")
        .cloned()
        .unwrap_or(json!(0));
    match &request_count {
        Value::Number(n) if n.as_i64().map(|v| v >= 0).unwrap_or(false) => {}
        _ => return Err(value("monitor observation request_count is invalid")),
    }
    match &render_request_count {
        Value::Number(n) if n.as_i64().map(|v| v >= 0).unwrap_or(false) => {}
        _ => return Err(value("monitor observation render_request_count is invalid")),
    }
    let failure_reason = item.get("failure_reason").cloned().unwrap_or(Value::Null);
    if !failure_reason.is_null() {
        let valid = failure_reason
            .as_str()
            .map(|text| !text.trim().is_empty() && text.chars().count() <= 512)
            .unwrap_or(false);
        if !valid {
            return Err(value("monitor observation failure_reason is invalid"));
        }
    }
    if qualifier == "failed" && failure_reason.is_null() {
        return Err(value("failed monitor observation requires failure_reason"));
    }
    let measurement = item.get("measurement").cloned().unwrap_or(Value::Null);
    if !measurement.is_null() {
        let valid = measurement
            .as_object()
            .map(|m| {
                m.keys().all(|key| TRACKED_FIELDS.contains(&key.as_str()))
                    && dumps_default(&measurement).len() <= 64 * 1024
            })
            .unwrap_or(false);
        if !valid {
            return Err(value("monitor observation measurement is invalid"));
        }
    }
    let mut changes = Vec::new();
    for change in item["changes"].as_array().cloned().unwrap_or_default() {
        let ok = change
            .as_object()
            .map(|_| {
                SEVERITIES.contains(
                    &change
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
            })
            .unwrap_or(false);
        if !ok {
            return Err(value("monitor changes need a declared severity"));
        }
        changes.push(change);
    }
    Ok(json!({
        "url": url,
        "changes": changes,
        "qualifier": qualifier,
        "measurement": measurement,
        "request_count": request_count,
        "render_request_count": render_request_count,
        "cache_state": cache_state,
        "failure_reason": failure_reason
            .as_str()
            .map(|text| Value::String(text.trim().to_string()))
            .unwrap_or(Value::Null),
        "source": source(item.get("source").unwrap_or(&Value::Null), root)?,
        "measured_at": now(),
    }))
}

pub(super) fn previous_measurement(document: &Value, url: &Value) -> Option<(Value, Value)> {
    for earlier in document["runs"].as_array()?.iter().rev() {
        if let Some(observations) = earlier.get("observations").and_then(Value::as_array) {
            for obs in observations.iter().rev() {
                if obs.get("url").map(|u| py_eq(u, url)).unwrap_or(false)
                    && ["fresh", "revalidated"].contains(
                        &obs.get("qualifier")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                    && obs
                        .get("measurement")
                        .map(Value::is_object)
                        .unwrap_or(false)
                {
                    return Some((
                        obs["measurement"].clone(),
                        earlier.get("scan_id").cloned().unwrap_or(Value::Null),
                    ));
                }
            }
        }
    }
    None
}

/// `_active_alerts`: unresolved per-field alerts plus their original measured value.
pub(super) fn active_alerts(document: &Value, url: &Value) -> BTreeMap<String, Value> {
    let mut active: BTreeMap<String, Value> = BTreeMap::new();
    for earlier in document["runs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        for alert in earlier
            .get("alerts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if !alert.get("url").map(|u| py_eq(u, url)).unwrap_or(false) {
                continue;
            }
            for change in alert
                .get("changes")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let Some(field) = change.get("field").and_then(Value::as_str) else {
                    continue;
                };
                let before = active
                    .get(field)
                    .and_then(|e| e.get("original_before").cloned())
                    .unwrap_or_else(|| change.get("before").cloned().unwrap_or(Value::Null));
                active.insert(
                    field.to_string(),
                    json!({"change": change, "original_before": before}),
                );
            }
        }
        for recovery in earlier
            .get("recoveries")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if recovery.get("url").map(|u| py_eq(u, url)).unwrap_or(false) {
                if let Some(field) = recovery
                    .get("change")
                    .filter(|c| c.is_object())
                    .and_then(|c| c.get("field"))
                    .and_then(Value::as_str)
                {
                    active.remove(field);
                }
            }
        }
    }
    active
}

/// `_measurement_changes`: compare only two measured values.
pub(super) fn measurement_changes(
    document: &Value,
    observation: &Value,
) -> ProjectsResult<Vec<Value>> {
    let qualifier = observation["qualifier"].as_str().unwrap_or_default();
    if ["stale", "unavailable", "partial", "failed"].contains(&qualifier) {
        return Ok(Vec::new());
    }
    let current = &observation["measurement"];
    let Some((baseline, baseline_scan_id)) = previous_measurement(document, &observation["url"])
    else {
        return Ok(Vec::new());
    };
    if current.is_null() {
        return Ok(Vec::new());
    }
    let mut changes = Vec::new();
    let active = active_alerts(document, &observation["url"]);
    for field in TRACKED_FIELDS {
        if current.get(field).is_none()
            || baseline.get(field).is_none()
            || current[field] == baseline[field]
        {
            continue;
        }
        if let Some(prior) = active.get(field) {
            if current[field] == prior["original_before"] {
                changes.push(json!({
                    "kind": "recovered",
                    "field": field,
                    "before": baseline[field],
                    "after": current[field],
                    "baseline_scan_id": baseline_scan_id,
                    "alert_baseline_scan_id": prior["change"].get("baseline_scan_id").cloned().unwrap_or(Value::Null),
                    "severity": prior["change"].get("severity").cloned().unwrap_or(json!("warning")),
                }));
                continue;
            }
        }
        changes.push(json!({
            "kind": format!("{field}_changed"),
            "field": field,
            "before": baseline[field],
            "after": current[field],
            "baseline_scan_id": baseline_scan_id,
            "severity": if ["status", "indexability", "canonical", "robots"].contains(&field) {
                "warning"
            } else {
                "notice"
            },
        }));
    }
    Ok(changes)
}

/// `_recent_alerts`: suppress repeats for a bounded number of completed passes.
pub(super) fn recent_alerts(document: &Value, suppression_runs: usize) -> HashSet<String> {
    let mut completed = Vec::new();
    for earlier in document["runs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .rev()
    {
        let coverage = earlier.get("coverage");
        let complete = match coverage {
            Some(c) if c.is_object() => py_truthy(&c["complete"]),
            _ => earlier.get("state").and_then(Value::as_str) != Some("partial"),
        };
        if complete && earlier.get("state").and_then(Value::as_str) != Some("failed") {
            completed.push(earlier);
        }
        if completed.len() == suppression_runs {
            break;
        }
    }
    completed
        .iter()
        .flat_map(|earlier| {
            earlier
                .get("alerts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|alert| {
            alert
                .get("changes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .map(|change| change_key(&change))
        .collect()
}

/// `_coverage`: make missing and failed measurement evidence explicit.
pub(super) fn coverage(planned_urls: &[Value], observations: &[Value]) -> Value {
    let observed_urls: Vec<Value> = observations
        .iter()
        .map(|item| item["url"].clone())
        .collect();
    let mut counts: BTreeMap<&str, usize> = QUALIFIERS.iter().map(|q| (*q, 0)).collect();
    for item in observations {
        if let Some(qualifier) = item["qualifier"].as_str() {
            *counts.entry(qualifier).or_insert(0) += 1;
        }
    }
    let unobserved: Vec<Value> = planned_urls
        .iter()
        .filter(|url| !observed_urls.contains(url))
        .cloned()
        .collect();
    let failures: Vec<Value> = observations
        .iter()
        .filter(|item| item["qualifier"].as_str() == Some("failed"))
        .map(|item| {
            json!({
                "url": item["url"],
                "reason": item.get("failure_reason").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let complete = unobserved.is_empty()
        && ["stale", "unavailable", "partial", "failed"]
            .iter()
            .all(|q| counts.get(q).copied().unwrap_or_default() == 0);
    json!({
        "planned_url_count": planned_urls.len(),
        "observed_url_count": observations.len(),
        "unobserved_url_count": unobserved.len(),
        "unobserved_urls": unobserved,
        "fresh_count": counts["fresh"],
        "revalidated_count": counts["revalidated"],
        "stale_count": counts["stale"],
        "unavailable_count": counts["unavailable"],
        "partial_count": counts["partial"],
        "failed_count": counts["failed"],
        "failures": failures,
        "complete": complete,
    })
}
