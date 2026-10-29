use super::*;

/// `_source_readiness`: explicit access state preserved, nothing guessed.
pub fn source_readiness(entry: &Value, as_of: Option<Date>) -> ProjectsResult<Value> {
    const NO_ACCESS: &[&str] = &["no_access", "invalid_grant", "insufficient_scope"];
    let source = entry["source"].as_str().unwrap_or_default();
    let end = available_end(source, as_of)?;
    let start = end
        .checked_sub(Span::new().days(27))
        .map_err(|e| value(e.to_string()))?;
    let expected: BTreeSet<String> = (0..28)
        .filter_map(|offset| {
            start
                .checked_add(Span::new().days(offset))
                .ok()
                .map(date_iso)
        })
        .collect();
    let coverage = &entry["coverage"];
    let data_days: BTreeSet<String> = coverage["complete"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d.as_str().map(str::to_owned))
        .filter(|d| expected.contains(d))
        .collect();
    let mut covered_days = data_days.clone();
    for day in coverage["empty"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d.as_str().map(str::to_owned))
    {
        if expected.contains(&day) {
            covered_days.insert(day);
        }
    }
    let mut access_failures: BTreeSet<String> = BTreeSet::new();
    if let Some(attempts) = entry["last_attempts"].as_object() {
        for (day, state) in attempts {
            if expected.contains(day) && state.as_str().is_some_and(|s| NO_ACCESS.contains(&s)) {
                access_failures.insert(day.clone());
            }
        }
    }
    let state = if !access_failures.is_empty() {
        "no_access"
    } else if covered_days.len() == 28 && !data_days.is_empty() {
        "data_28d"
    } else if covered_days.len() == 28 {
        "empty_28d"
    } else if !data_days.is_empty() {
        "partial_28d"
    } else {
        "no_recent_data"
    };
    Ok(json!({
        "state": state,
        "data_28d": !data_days.is_empty(),
        "coverage_28d": covered_days.len() == 28,
        "data_days": data_days.len(),
        "available_days": covered_days.len(),
        "expected_days": 28,
        "window": {"start": date_iso(start), "end": date_iso(end)},
        "access_state": if access_failures.is_empty() { "unknown" } else { "no_access" },
        "access_failure_days": access_failures.iter().collect::<Vec<_>>(),
        "sources": [source],
    }))
}

/// `cloudflare_history.list_zones(project, limit=100, offset=0)` — read-only.
pub(super) fn list_zones(project: &Path, limit: i64, offset: i64) -> ProjectsResult<Value> {
    if !(1..=100).contains(&limit) || offset < 0 {
        return Err(value("limit must be 1..100 and offset must be nonnegative"));
    }
    if project.is_symlink() || !project.is_dir() {
        return Err(value("project must be a regular directory"));
    }
    let manifest = project.join("project.json");
    let manifest_meta = fs::symlink_metadata(&manifest);
    let ok_manifest = manifest_meta
        .as_ref()
        .map(|m| m.is_file() && !m.file_type().is_symlink())
        .unwrap_or(false);
    let ok_manifest = ok_manifest
        && spiderhead_core::filesystem::link_identity(&manifest)
            .map(|i| i.2 == 1)
            .unwrap_or(false);
    if !ok_manifest || manifest.is_symlink() {
        return Err(value("project requires a regular, unaliased project.json"));
    }
    let path = project.join("cloudflare.sqlite");
    let info = fs::symlink_metadata(&path)?;
    {
        use spiderhead_core::filesystem::{link_identity, StatCompat};
        let unaliased = link_identity(&path).map(|i| i.2 == 1).unwrap_or(false);
        if !info.is_file() || !unaliased || info.mode() & 0o077 != 0 {
            return Err(value(
                "Cloudflare history must be a private regular unaliased file",
            ));
        }
    }
    let con = Connection::open_with_flags(
        path.canonicalize().unwrap_or(path.clone()),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| value(e.to_string()))?;
    con.execute_batch("PRAGMA trusted_schema=OFF")
        .map_err(|e| value(e.to_string()))?;
    con.execute_batch("BEGIN")
        .map_err(|e| value(e.to_string()))?;
    let tables: BTreeSet<String> = {
        let mut stmt = con
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|e| value(e.to_string()))?;
        let mapped = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| value(e.to_string()))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    if tables
        != ["cloudflare_meta", "cloudflare_snapshots"]
            .into_iter()
            .map(String::from)
            .collect()
    {
        return Err(value(
            "existing database is not a Cloudflare history database",
        ));
    }
    let formats: Vec<String> = {
        let mut stmt = con
            .prepare("SELECT format FROM cloudflare_meta")
            .map_err(|e| value(e.to_string()))?;
        let mapped = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| value(e.to_string()))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    if formats != [CLOUDFLARE_FORMAT.to_owned()] {
        return Err(value("unsupported Cloudflare history format"));
    }
    con.execute_batch("PRAGMA query_only=ON")
        .map_err(|e| value(e.to_string()))?;
    let total: i64 = con
        .query_row(
            "SELECT COUNT(DISTINCT zone_id) FROM cloudflare_snapshots",
            [],
            |row| row.get(0),
        )
        .map_err(|e| value(e.to_string()))?;
    let zone_ids: Vec<String> = {
        let mut stmt = con
            .prepare(
                "SELECT zone_id FROM cloudflare_snapshots GROUP BY zone_id \
                 ORDER BY MAX(id) DESC LIMIT ? OFFSET ?",
            )
            .map_err(|e| value(e.to_string()))?;
        let mapped = stmt
            .query_map((limit, offset), |row| row.get::<_, String>(0))
            .map_err(|e| value(e.to_string()))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    let mut zones = Vec::new();
    for zone_id in zone_ids {
        let row = con
            .query_row(
                "SELECT zone,observed_at,period_from,period_to,state,sampled,truncated_days,\
                 payload FROM cloudflare_snapshots WHERE zone_id=? ORDER BY id DESC LIMIT 1",
                [zone_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| value(e.to_string()))?;
        if let Some((zone, observed_at, from, to, state, sampled, truncated_days, payload)) = row {
            let truncated_days_parsed: bool = serde_json::from_str(&truncated_days)
                .map(|v: Value| !crate::schedule::falsy(&v))
                .unwrap_or(false);
            let payload_truncated: bool = serde_json::from_str::<Value>(&payload)
                .ok()
                .and_then(|v| v.get("url_facts_truncated").cloned())
                .map(|v| !crate::schedule::falsy(&v))
                .unwrap_or(false);
            zones.push(json!({
                "zone_id": zone_id,
                "zone": zone,
                "latest_observed_at": observed_at,
                "latest_period": {"from": from, "to": to},
                "state": state,
                "sampled": sampled != 0,
                "truncated": truncated_days_parsed || payload_truncated,
            }));
        }
    }
    Ok(json!({
        "schema_version": CLOUDFLARE_FORMAT,
        "total": total,
        "offset": offset,
        "limit": limit,
        "zones": zones,
    }))
}

/// `_cloudflare_history_state`: partial snapshots stay unavailable.
pub fn cloudflare_history_state(row: &Value, as_of: Date) -> Value {
    let observed_text = py_str(&row["latest_observed_at"]);
    let observed_day = if observed_text == "None" && row["latest_observed_at"].is_null() {
        None
    } else {
        {
            let text = observed_text.replace('Z', "+00:00");
            text.parse::<jiff::Timestamp>()
                .ok()
                .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date())
                .or_else(|| {
                    text.parse::<jiff::civil::DateTime>().ok().and_then(|dt| {
                        dt.to_zoned(crate::pytime::system_zone())
                            .ok()
                            .map(|z| z.date())
                    })
                })
        }
    };
    let period = &row["latest_period"];
    let (start, end): (Value, Value) = if period.is_object() {
        (
            period.get("from").cloned().unwrap_or(Value::Null),
            period.get("to").cloned().unwrap_or(Value::Null),
        )
    } else {
        (Value::Null, Value::Null)
    };
    let period_start = parse_date(&py_str(&start).chars().take(10).collect::<String>());
    let period_end = parse_date(&py_str(&end).chars().take(10).collect::<String>());
    let since27 = as_of.checked_sub(Span::new().days(27)).unwrap_or(as_of);
    let since1 = as_of.checked_sub(Span::new().days(1)).unwrap_or(as_of);
    let recent = observed_day.is_some_and(|d| since27 <= d && d <= as_of);
    let complete_period = period_start.is_some()
        && period_end.is_some()
        && period_start.is_some_and(|s| s <= since27)
        && period_end.is_some_and(|e| e >= since1);
    let sampled = row["sampled"].as_bool().unwrap_or(false);
    let truncated = row["truncated"].as_bool().unwrap_or(false);
    let complete =
        row["state"].as_str() == Some("complete") && complete_period && !sampled && !truncated;
    let row_state = row["state"].as_str().unwrap_or_default();
    let state = if recent && complete {
        "data_28d"
    } else if recent && (row_state == "complete" || row_state == "partial") {
        "partial_28d"
    } else if observed_day.is_some() {
        "stale"
    } else {
        "no_history"
    };
    let mut result = json!({
        "service": "cloudflare",
        "resource": row["zone_id"],
        "history_state": state,
        "data_28d": recent,
        "coverage_28d": recent && complete,
        "data_days": null,
        "available_days": null,
        "expected_days": 28,
        "window": {"start": start, "end": end},
        "access_state": "unknown",
        "sources": ["cloudflare"],
    });
    if !crate::schedule::falsy(&row["zone"]) {
        result["label"] = row["zone"].clone();
    }
    result
}

/// `_history_resources` — `(output, cloudflare_has_more)`.
pub(super) fn history_resources(
    root: &Path,
    as_of: Date,
    source_as_of: Option<Date>,
) -> ProjectsResult<(Vec<Value>, bool)> {
    let path = root.join("sources.sqlite");
    let mut output: Vec<Value> = Vec::new();
    if fs::symlink_metadata(&path).is_ok() {
        if path.is_symlink() || !path.is_file() {
            return Err(value("sources.sqlite is unsafe"));
        }
        let history = sources_status(&path)?;
        // Keep (service, resource) pairs in first-appearance order for stable output.
        let mut grouped: Vec<((String, String), Vec<Value>)> = Vec::new();
        for entry in &history {
            let service = match entry["source"].as_str().unwrap_or_default() {
                "webmaster_history" => "webmaster",
                s => s,
            };
            if !matches!(
                entry["source"].as_str(),
                Some("gsc" | "ga4" | "metrika" | "webmaster" | "webmaster_history")
            ) {
                continue;
            }
            let resource = entry["resource"].as_str().unwrap_or_default().to_owned();
            let key = (service.to_owned(), resource);
            if let Some((_, entries)) = grouped.iter_mut().find(|(k, _)| k == &key) {
                entries.push(entry.clone());
            } else {
                grouped.push((key, vec![entry.clone()]));
            }
        }
        for ((service, resource), entries) in grouped {
            let summaries: Vec<Value> = entries
                .iter()
                .map(|e| source_readiness(e, source_as_of))
                .collect::<ProjectsResult<Vec<_>>>()?;
            let failures: Vec<&Value> = summaries
                .iter()
                .filter(|s| s["state"].as_str() == Some("no_access"))
                .collect();
            let data_sources: Vec<&Value> = summaries
                .iter()
                .filter(|s| s["data_28d"].as_bool() == Some(true))
                .collect();
            let best: &Value = if !failures.is_empty() {
                failures[0]
            } else if !data_sources.is_empty() {
                data_sources[0]
            } else {
                summaries
                    .iter()
                    .max_by_key(|s| s["available_days"].as_i64().unwrap_or(0))
                    .unwrap_or(&summaries[0])
            };
            let mut sources: Vec<String> = summaries
                .iter()
                .filter_map(|s| s["sources"][0].as_str().map(str::to_owned))
                .collect();
            sources.sort();
            output.push(json!({
                "service": service,
                "resource": resource,
                "history_state": best["state"],
                "data_28d": best["data_28d"],
                "coverage_28d": best["coverage_28d"],
                "data_days": best["data_days"],
                "available_days": best["available_days"],
                "expected_days": best["expected_days"],
                "window": best["window"],
                "access_state": best["access_state"],
                "sources": sources,
            }));
        }
    }
    let mut cloudflare_has_more = false;
    let cloudflare_path = root.join("cloudflare.sqlite");
    if fs::symlink_metadata(&cloudflare_path).is_ok() {
        if cloudflare_path.is_symlink() || !cloudflare_path.is_file() {
            return Err(value("cloudflare.sqlite is unsafe"));
        }
        let local = list_zones(root, 100, 0)?;
        let zones = local["zones"].as_array().cloned().unwrap_or_default();
        cloudflare_has_more = local["total"].as_i64().unwrap_or(0) > zones.len() as i64;
        for row in zones {
            if row.get("zone_id").is_some_and(|z| z.is_string()) {
                output.push(cloudflare_history_state(&row, as_of));
            }
        }
    }
    Ok((output, cloudflare_has_more))
}

/// `list_bindings`.
pub fn list_bindings(directory: &Path, as_of: Option<&str>) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let parsed = match as_of {
        Some(text) => Some(
            text.parse::<Date>()
                .map_err(|_| value("as_of must be an ISO calendar date"))?,
        ),
        None => None,
    };
    let source_as_of = parsed;
    let as_of_day = parsed.unwrap_or_else(|| Zoned::now().datetime().date());
    let (known, resources_truncated) = history_resources(&root, as_of_day, source_as_of)?;
    let mut selected: Vec<Value> = read_bindings(&root)?.iter().map(view).collect();
    for item in &mut selected {
        let history = known
            .iter()
            .find(|k| k["service"] == item["service"] && k["resource"] == item["resource"]);
        let readiness = match history {
            Some(h) => {
                let mut out = h.as_object().cloned().unwrap_or_default();
                out.shift_remove("service");
                out.shift_remove("resource");
                Value::Object(out)
            }
            None => json!({
                "history_state": "no_history",
                "data_28d": null,
                "coverage_28d": null,
                "data_days": 0,
                "available_days": 0,
                "expected_days": 28,
                "access_state": "unknown",
                "reason": "no local sync history for this binding",
            }),
        };
        item["readiness"] = readiness;
    }
    let selected_pairs: Vec<(Value, Value)> = selected
        .iter()
        .map(|i| (i["service"].clone(), i["resource"].clone()))
        .collect();
    let resources: Vec<Value> = known
        .iter()
        .map(|item| {
            let mut out = item.as_object().cloned().unwrap_or_default();
            let pair = (item["service"].clone(), item["resource"].clone());
            out.insert(
                "status".into(),
                json!(if selected_pairs.contains(&pair) {
                    "selected"
                } else {
                    "not_selected"
                }),
            );
            Value::Object(out)
        })
        .collect();
    Ok(json!({
        "ok": true,
        "path": root.to_string_lossy(),
        "count": selected.len(),
        "bindings": selected,
        "resources": resources,
        "resources_truncated": resources_truncated,
        "as_of": date_iso(as_of_day),
    }))
}

/// `link`: bind one resource ID to the project; relinking the same pair changes nothing.
pub fn link(
    directory: &Path,
    service_name: &str,
    resource_id: &str,
    label_text: Option<&str>,
) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let service = service(&json!(service_name))?;
    let resource = resource(&json!(resource_id))?;
    let label = label(&label_text.map(|l| json!(l)).unwrap_or(Value::Null))?;
    let mut bindings = read_bindings(&root)?;
    for item in &bindings {
        if item["service"].as_str() == Some(service.as_str())
            && item["resource"].as_str() == Some(resource.as_str())
        {
            return Ok(json!({
                "ok": true,
                "path": root.to_string_lossy(),
                "created": false,
                "binding": view(item),
            }));
        }
    }
    if bindings.len() >= MAX_BINDINGS {
        return Err(value(format!(
            "a project holds at most {MAX_BINDINGS} source bindings"
        )));
    }
    let binding = json!({
        "service": service,
        "resource": resource,
        "label": label,
        "origin": "manual",
        "linked_at": crate::pytime::now_iso_micros(),
    });
    bindings.push(binding.clone());
    write_bindings(&root, &bindings)?;
    Ok(json!({
        "ok": true,
        "path": root.to_string_lossy(),
        "created": true,
        "binding": view(&binding),
    }))
}

/// `unlink`: remove one binding; an unknown pair is a clear error, not a crash.
pub fn unlink(directory: &Path, service_name: &str, resource_id: &str) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let service = service(&json!(service_name))?;
    let resource = resource(&json!(resource_id))?;
    let bindings = read_bindings(&root)?;
    let kept: Vec<Value> = bindings
        .iter()
        .filter(|i| {
            !(i["service"].as_str() == Some(service.as_str())
                && i["resource"].as_str() == Some(resource.as_str()))
        })
        .cloned()
        .collect();
    if kept.len() == bindings.len() {
        return Err(value("no such source binding in this project"));
    }
    write_bindings(&root, &kept)?;
    Ok(json!({
        "ok": true,
        "path": root.to_string_lossy(),
        "removed": {"service": service, "resource": resource},
    }))
}
