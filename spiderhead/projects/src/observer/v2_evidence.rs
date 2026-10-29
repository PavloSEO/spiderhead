use super::*;

/// The audit.v2 branch of `_read_scan_evidence`: findings/skip/coverage update.
pub(super) fn v2_evidence(scan_path: &Path) -> ProjectsResult<Value> {
    let reader = spiderhead_store::audit_v2::AuditV2Reader::new(scan_path, true, true, None)
        .map_err(ProjectsError::Store)?;
    let total = reader.count("/issues").map_err(ProjectsError::Store)?;
    let collections = reader.collections.clone();
    let header = reader.header.clone();
    let con = open_companion(&reader.path)?;
    let query_group = |field: &str| -> ProjectsResult<Map<String, Value>> {
        let mut statement = con
            .prepare(&format!(
                "SELECT json_extract(value_json,'$.{field}'),COUNT(*) FROM items \
                 WHERE pointer='/issues' GROUP BY json_extract(value_json,'$.{field}')"
            ))
            .map_err(|e| value(e.to_string()))?;
        let rows = statement
            .query_map([], |r| {
                Ok((r.get::<_, rusqlite::types::Value>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| value(e.to_string()))?;
        let mut out = Map::new();
        for row in rows {
            let (key, count) = row.map_err(|e| value(e.to_string()))?;
            if let rusqlite::types::Value::Text(text) = key {
                out.insert(text, json!(count));
            }
        }
        Ok(out)
    };
    let by_severity = query_group("severity")?;
    let by_check = query_group("check")?;
    let mut statement = con
        .prepare("SELECT value_json FROM items WHERE pointer='/issues' ORDER BY ordinal LIMIT 20")
        .map_err(|e| value(e.to_string()))?;
    let first_issues: Vec<Value> = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| value(e.to_string()))?
        .map(|row| {
            let text = row.map_err(|e| value(e.to_string()))?;
            serde_json::from_str(&text).map_err(|e| value(e.to_string()))
        })
        .collect::<ProjectsResult<Vec<_>>>()?;
    let first_sizes = v2_group_sizes(
        &con,
        &first_issues
            .iter()
            .map(|issue| {
                issue
                    .get("group_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>(),
    )?;
    let first: Vec<Value> = first_issues
        .iter()
        .enumerate()
        .map(|(ordinal, issue)| {
            finding_summary(
                issue,
                ordinal as i64,
                issue
                    .get("group_id")
                    .and_then(Value::as_str)
                    .and_then(|gid| first_sizes.get(gid).copied()),
                false,
            )
        })
        .collect::<ProjectsResult<Vec<_>>>()?;
    let mut skipped = header
        .get("run")
        .and_then(|r| r.get("checks_skipped"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    if collections.contains_key("/run/checks_skipped") {
        skipped = json!(reader
            .iter_collection("/run/checks_skipped")
            .map_err(ProjectsError::Store)?);
    }
    let coverage_input = header
        .get("summary")
        .and_then(|s| s.get("check_coverage"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "findings": {
            "state": "available",
            "reason": null,
            "total": total,
            "by_severity": by_severity,
            "by_check": by_check,
            "items": first,
            "truncated": total as usize > first.len(),
            "next_offset": if total as usize > first.len() {
                json!(first.len())
            } else {
                Value::Null
            },
        },
        "skipped_checks": skipped,
        "check_coverage": spiderhead_store::issue_counters::coverage_summary(&coverage_input),
    }))
}

/// `_scan_evidence`: one retained projection per unchanged scan/audit state.
pub(crate) fn scan_evidence(
    row: &Map<String, Value>,
    segments: Option<&[Value]>,
) -> ProjectsResult<Value> {
    let key = evidence_cache_key(row)?;
    let state = evidence_state(row, segments);
    if let Some(cached) = lru_get(evidence_cache(), &key) {
        let trigger = classify_trigger(cached.get("state"), &state)?;
        let (recompute, _) = needs_recompute(trigger)?;
        if !recompute {
            return Ok(cached["evidence"].clone());
        }
    }
    let evidence = read_scan_evidence(row)?;
    if evidence.get("state").and_then(Value::as_str) != Some("available") {
        return Ok(evidence);
    }
    // Do not retain an observation read while its source was being changed.
    if evidence_cache_key(row)? != key {
        return Ok(evidence);
    }
    lru_put(
        evidence_cache(),
        evidence_cache_limit(),
        key,
        json!({"evidence": evidence, "state": state}),
    );
    Ok(evidence)
}

/// `_site_scans`: bound one site's retained scans with artifact-tied evidence.
pub(super) fn site_scans(root: &Path, scans: &Value, limit: i64) -> ProjectsResult<Value> {
    let items_in = obj_get(scans, "items")?
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut items = Vec::new();
    for row in items_in.iter().take(limit.max(0) as usize) {
        let Some(map) = row.as_object() else {
            continue;
        };
        let path = obj_get(row, "path")?.as_str().unwrap_or_default();
        let artifact = relative_artifact(root, path);
        let mut item = map.clone();
        item.remove("path");
        match artifact {
            None => {
                item.insert(
                    "artifact".into(),
                    json!({
                        "state": "unavailable",
                        "reason": "retained scan path is outside the project workspace",
                    }),
                );
                item.insert("evidence".into(), item["artifact"].clone());
            }
            Some(artifact_path) => {
                item.insert(
                    "artifact".into(),
                    json!({"state": "available", "path": artifact_path}),
                );
                item.insert("evidence".into(), scan_evidence(map, None)?);
            }
        }
        items.push(Value::Object(item));
    }
    let total = obj_get(scans, "total")?;
    let total_i = total
        .as_i64()
        .ok_or_else(|| py_named("TypeError", "comparison between incompatible types"))?;
    Ok(json!({
        "total": total,
        "shown": items.len(),
        "has_more": !falsy(&scans.get("has_more").cloned().unwrap_or(Value::Null))
            || total_i > items.len() as i64,
        "errors": obj_get(scans, "errors")?,
        "items": items,
    }))
}

/// `_site_projection`: one read-only own-site or competitor observation row.
#[allow(clippy::too_many_arguments)]
pub(super) fn site_projection(
    root: &Path,
    project: &Value,
    checklist: &Value,
    scans: &Value,
    role: &str,
    source: &Value,
    observed_at: &Value,
    candidate_state: &Value,
    directory: &str,
    scan_limit: i64,
    run_offset: i64,
    run_limit: i64,
) -> ProjectsResult<Value> {
    let runs = crate::run_observation::status_page(root, run_limit, run_offset)?;
    Ok(json!({
        "role": role,
        "project_uuid": obj_get(project, "project_uuid")?,
        "directory": directory,
        "site": obj_get(project, "site")?,
        "candidate": if role == "competitor" {
            json!({
                "state": candidate_state,
                "source": source,
                "observed_at": observed_at,
            })
        } else {
            Value::Null
        },
        "coverage": coverage_summary(checklist),
        "methods": method_coverage(checklist),
        "scans": site_scans(root, scans, scan_limit)?,
        "runs": runs,
    }))
}

/// `_competitor_sites`: read declared competitor workspaces only.
pub(super) fn competitor_sites(
    root: &Path,
    preparation: &Value,
    scan_limit: i64,
    coverage_views: Option<&Map<String, Value>>,
    run_offset: i64,
    run_limit: i64,
) -> ProjectsResult<Vec<Value>> {
    let mut sites = Vec::new();
    for candidate in preparation
        .get("competitors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let attempt = (|| -> ProjectsResult<Value> {
            let directory = candidate.get("directory");
            let Some(relative) = directory.and_then(Value::as_str) else {
                return Err(value("competitor workspace reference is unavailable"));
            };
            let child = root.join(relative);
            let (child_root, child_project) = crate::workspace::load(&child)?;
            if obj_get(&child_project, "project_uuid")?
                != candidate.get("project_uuid").unwrap_or(&Value::Null)
            {
                return Err(value("competitor project identity mismatch"));
            }
            if obj_get(obj_get(&child_project, "site")?, "target")?.as_str()
                != Some(
                    crate::workspace::target(
                        candidate
                            .get("url")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )?
                    .as_str(),
                )
            {
                return Err(value("competitor site identity mismatch"));
            }
            let checklist = match coverage_views
                .and_then(|v| v.get(&child_root.to_string_lossy().to_string()))
            {
                Some(view) => view.clone(),
                None => crate::coverage::coverage_status(&child_root, false)?,
            };
            site_projection(
                &child_root,
                &child_project,
                &checklist,
                &json!(spiderhead_store::history::list_scans(
                    &child_root.join("scans"),
                    0,
                    100
                )?),
                "competitor",
                candidate.get("source").unwrap_or(&Value::Null),
                candidate.get("observed_at").unwrap_or(&Value::Null),
                candidate.get("state").unwrap_or(&Value::Null),
                relative,
                scan_limit,
                run_offset,
                run_limit,
            )
        })();
        match attempt {
            Ok(site) => sites.push(site),
            Err(err) if site_error_caught(&err) => {
                let reason_text = reason(&err);
                sites.push(json!({
                    "role": "competitor",
                    "project_uuid": candidate.get("project_uuid").cloned().unwrap_or(Value::Null),
                    "directory": candidate.get("directory").cloned().unwrap_or(Value::Null),
                    "site": {"target": candidate.get("url").cloned().unwrap_or(Value::Null), "host": null, "label": null},
                    "candidate": {
                        "state": candidate.get("state").cloned().unwrap_or(Value::Null),
                        "source": candidate.get("source").cloned().unwrap_or(Value::Null),
                        "observed_at": candidate.get("observed_at").cloned().unwrap_or(Value::Null),
                    },
                    "coverage": {
                        "state": "unavailable",
                        "reason": reason_text,
                        "complete": null,
                        "counts": null,
                        "axes": {},
                    },
                    "methods": {
                        "state": "unavailable",
                        "reason": reason_text,
                        "kinds": {},
                    },
                    "scans": {
                        "total": null,
                        "shown": 0,
                        "has_more": false,
                        "errors": [],
                        "items": [],
                    },
                }));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(sites)
}

/// The I/O and value errors skipped here; anything else propagates.
pub(super) fn site_error_caught(err: &ProjectsError) -> bool {
    matches!(
        py_exception_name(err).0,
        "OSError"
            | "FileNotFoundError"
            | "FileExistsError"
            | "PermissionError"
            | "NotADirectoryError"
            | "IsADirectoryError"
            | "ValueError"
            | "JSONDecodeError"
    )
}

/// `findings_page`: browse a retained scan's findings with bounded pagination.
#[allow(clippy::too_many_arguments)]
pub fn findings_page(
    directory: &Path,
    scan_uuid: Option<&str>,
    offset: i64,
    limit: i64,
    query: &str,
    url: Option<&str>,
    sort: &str,
    descending: bool,
) -> ProjectsResult<Value> {
    if offset < 0 {
        return Err(value("finding offset must be non-negative"));
    }
    if !(1..=MAX_FINDING_PAGE_SIZE).contains(&limit) {
        return Err(value("finding limit must be from 1 to 100"));
    }
    if query.chars().count() > 256 {
        return Err(value(
            "finding query must be text of at most 256 characters",
        ));
    }
    if let Some(text) = url {
        if text.is_empty() || text.chars().count() > 8192 {
            return Err(value(
                "finding URL must be exact nonempty text of at most 8192 characters",
            ));
        }
    }
    if !FINDING_SORTS.contains(&sort) {
        return Err(value(
            "finding sort must be severity, check, target_url or id",
        ));
    }
    let row = scan_row(directory, scan_uuid)?;
    let row_value = Value::Object(row.clone());
    let path = obj_get(&row_value, "path")?.as_str().unwrap_or_default();
    if spiderhead_store::audit_v2::audit_v2_path(Path::new(path)).exists() {
        let page = v2_findings_page(&row, offset, limit, query, url, sort, descending)?;
        return finding_page_result(
            &row,
            query,
            sort,
            descending,
            page.total,
            page.matched,
            &page.selected,
            offset,
            limit,
            Some(&page.group_sizes),
            &page.via_group_ids,
        );
    }
    let issues = retained_findings(&row)?["issues"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let needle = icu_casemap::CaseMapper::new()
        .fold_string(query)
        .to_string()
        .trim()
        .to_string();
    let mut indexed: Vec<(i64, Value)> = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| (i as i64, issue.clone()))
        .collect();
    if !needle.is_empty() {
        indexed.retain(|(_, issue)| {
            let hay = [
                "id",
                "check",
                "severity",
                "target_url",
                "url",
                "message",
                "text",
            ]
            .iter()
            .map(|key| {
                issue
                    .get(*key)
                    .map(|v| {
                        if v.is_string() {
                            v.as_str().unwrap_or_default().to_string()
                        } else {
                            py_str(v)
                        }
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
            icu_casemap::CaseMapper::new()
                .fold_string(&hay)
                .contains(needle.as_str())
        });
    }
    let mut member_group_ids = BTreeSet::new();
    let mut groups: Option<Vec<Value>> = None;
    if let Some(url_text) = url {
        // Membership, not only the finding's own target row, decides a URL lookup.
        let collected = legacy_groups(&row);
        for group in &collected {
            if let Some(group_id) = group.get("group_id").and_then(Value::as_str) {
                if group
                    .get("urls")
                    .and_then(Value::as_array)
                    .map(|urls| urls.iter().any(|u| u.as_str() == Some(url_text)))
                    .unwrap_or(false)
                {
                    member_group_ids.insert(group_id.to_string());
                }
            }
        }
        groups = Some(collected);
        indexed.retain(|(_, issue)| {
            (issue.get("target_url").or_else(|| issue.get("url"))).and_then(Value::as_str)
                == Some(url_text)
                || issue
                    .get("group_id")
                    .and_then(Value::as_str)
                    .map(|gid| member_group_ids.contains(gid))
                    .unwrap_or(false)
        });
    }
    let casefold = |v: &Value| {
        icu_casemap::CaseMapper::new()
            .fold_string(&py_str(v))
            .to_string()
    };
    let key_of = |item: &(i64, Value)| -> (bool, i64, String, i64) {
        let (ordinal, issue) = item;
        let mut val = issue.get(sort).cloned().unwrap_or(Value::Null);
        if sort == "target_url" && falsy(&val) {
            val = issue.get("url").cloned().unwrap_or(Value::Null);
        }
        let missing = val.is_null() || val == json!("");
        if sort == "severity" {
            let name = if val.is_string() {
                casefold(&val)
            } else {
                py_str(&val)
            };
            (missing, severity_rank(&name), String::new(), *ordinal)
        } else {
            let text = if falsy(&val) {
                String::new()
            } else {
                casefold(&val)
            };
            (missing, 0, text, *ordinal)
        }
    };
    indexed.sort_by(|a, b| {
        let ka = key_of(a);
        let kb = key_of(b);
        let ord = ka.0.cmp(&kb.0).then(ka.3.cmp(&kb.3));
        let inner = if sort == "severity" {
            ka.1.cmp(&kb.1)
        } else {
            ka.2.cmp(&kb.2)
        };
        let full = ka.0.cmp(&kb.0).then(inner).then(ka.3.cmp(&kb.3));
        let _ = ord;
        if descending {
            full.reverse()
        } else {
            full
        }
    });
    indexed.sort_by_key(|item| key_of(item).0);
    let selected: Vec<(i64, Value)> = indexed
        .iter()
        .skip(offset.max(0) as usize)
        .take(limit as usize)
        .cloned()
        .collect();
    let wanted: Vec<Option<String>> = selected
        .iter()
        .filter(|(_, issue)| issue.is_object())
        .map(|(_, issue)| {
            issue
                .get("group_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(Option::is_some)
        .collect();
    let group_sizes = if wanted.is_empty() {
        BTreeMap::new()
    } else {
        let collected = match groups {
            Some(g) => g,
            None => legacy_groups(&row),
        };
        group_size_map(&collected, &wanted)
    };
    finding_page_result(
        &row,
        query,
        sort,
        descending,
        issues.len() as i64,
        indexed.len() as i64,
        &selected,
        offset,
        limit,
        Some(&group_sizes),
        &member_group_ids,
    )
}
