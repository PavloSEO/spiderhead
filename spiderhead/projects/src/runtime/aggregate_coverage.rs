use super::*;

/// `aggregate_coverage`: combine declared site checklists without cross-assigning evidence.
pub fn aggregate_coverage(
    directory: &str,
    primary: Value,
    views: Option<&mut Map<String, Value>>,
    verify_evidence: bool,
) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(Path::new(directory))?;
    let mut local_views: Map<String, Value> = Map::new();
    let views = match views {
        Some(v) => v,
        None => &mut local_views,
    };
    views.insert(root.to_string_lossy().to_string(), primary.clone());
    let preparation = preparation_status(&root)?;
    let children = preparation
        .get("competitors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if children.is_empty() {
        return Ok(primary);
    }
    let mut sites = vec![json!({
        "directory": ".",
        "project_uuid": project["project_uuid"],
        "site": project["site"]["target"],
        "checklist": primary,
    })];
    let mut seen: BTreeSet<String> = [project["project_uuid"]
        .as_str()
        .unwrap_or_default()
        .to_owned()]
    .into_iter()
    .collect();
    for child in &children {
        let relative = child.get("directory").and_then(Value::as_str).unwrap_or("");
        let resolved = (|| -> ProjectsResult<Value> {
            let rel_path = Path::new(relative);
            if relative.is_empty()
                || rel_path.is_absolute()
                || rel_path.components().any(|c| c.as_os_str() == "..")
                || !relative.starts_with("competitors/")
            {
                return Err(crate::error::value("unsafe competitor project reference"));
            }
            let path = root.join(rel_path);
            if path.is_symlink()
                || !path
                    .canonicalize()
                    .map_err(ProjectsError::Io)?
                    .starts_with(&root)
            {
                return Err(crate::error::value("unsafe competitor project reference"));
            }
            let (_, metadata) = workspace_load(&path)?;
            if metadata["project_uuid"].as_str()
                != child.get("project_uuid").and_then(Value::as_str)
                || metadata["site"]["target"].as_str().unwrap_or_default()
                    != competitor_identity(child["url"].as_str().unwrap_or_default())?
                || seen.contains(metadata["project_uuid"].as_str().unwrap_or_default())
            {
                return Err(crate::error::value("competitor project identity mismatch"));
            }
            Ok(metadata)
        })();
        match resolved {
            Err(e) => {
                sites.push(json!({
                    "directory": relative,
                    "project_uuid": child.get("project_uuid"),
                    "site": child.get("url"),
                    "checklist": {"state": "unavailable", "reason": e.to_string(), "counts": null},
                }));
                continue;
            }
            Ok(metadata) => {
                let path = root.join(relative);
                seen.insert(
                    metadata["project_uuid"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
                let child_key = path
                    .canonicalize()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !views.contains_key(&child_key) {
                    let status = crate::coverage::coverage_status(&path, verify_evidence)?;
                    views.insert(child_key.clone(), status);
                }
                sites.push(json!({
                    "directory": relative,
                    "project_uuid": metadata["project_uuid"],
                    "site": metadata["site"]["target"],
                    "checklist": views[&child_key],
                }));
            }
        }
    }
    let mut result = primary.clone();
    result["sites"] = Value::Array(
        sites
            .iter()
            .map(|site| {
                let mut row = Map::new();
                for key in ["directory", "project_uuid", "site"] {
                    row.insert(key.into(), site[key].clone());
                }
                row.insert(
                    "counts".into(),
                    site["checklist"]
                        .get("counts")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                row.insert("state".into(), site["checklist"]["state"].clone());
                if let Some(reason) = site["checklist"].get("reason") {
                    row.insert("reason".into(), reason.clone());
                }
                Value::Object(row)
            })
            .collect(),
    );
    if sites
        .iter()
        .any(|site| site["checklist"]["state"].as_str() != Some("initialized"))
    {
        result["state"] = json!("not_initialized");
        result["complete"] = json!(false);
        result["counts_known"] = json!(false);
        result["counts"] = Value::Null;
        result["by_kind"] = json!({});
        result["items"] = json!([]);
        result["views"] = Value::Object(
            primary
                .get("views")
                .and_then(Value::as_object)
                .map(|v| v.keys().map(|k| (k.clone(), json!([]))).collect())
                .unwrap_or_default(),
        );
        result["coverage"] = Value::Null;
        result["reason"] = json!("one or more site checklists are not initialized");
        return Ok(result);
    }
    // All sites initialized: merge counts, coverage axes, items with site prefixes.
    if let Some(counts) = primary.get("counts").and_then(Value::as_object) {
        let merged: Map<String, Value> = counts
            .keys()
            .map(|key| {
                let sum: i64 = sites
                    .iter()
                    .map(|s| s["checklist"]["counts"][key].as_i64().unwrap_or(0))
                    .sum();
                (key.clone(), json!(sum))
            })
            .collect();
        result["counts"] = Value::Object(merged);
    }
    if let Some(coverage) = primary.get("coverage").and_then(Value::as_object) {
        let mut axes = Map::new();
        for name in coverage.keys() {
            let mut merged = coverage[name].clone();
            for site in &sites[1..] {
                let axis = &site["checklist"]["coverage"][name];
                for key in ["numerator", "denominator", "measured_urls"] {
                    if merged.get(key).is_some() && axis.get(key).is_some() {
                        merged[key] = match (merged[key].as_i64(), axis[key].as_i64()) {
                            (Some(a), Some(b)) => json!(a + b),
                            _ => Value::Null,
                        };
                    }
                }
                for key in [
                    "excluded",
                    "pending_exclusion",
                    "not_agreed",
                    "unfinished",
                    "unverified_measurements",
                ] {
                    if merged.get(key).is_some() {
                        merged[key] = json!(
                            merged[key].as_i64().unwrap_or(0) + axis[key].as_i64().unwrap_or(0)
                        );
                    }
                }
                if merged.get("population_kind").is_some()
                    && (axis["population_kind"] != merged["population_kind"]
                        || axis["population_name"] != merged["population_name"])
                {
                    merged["population_kind"] = json!("mixed");
                    merged["population_name"] = Value::Null;
                }
                if axis["state"].as_str() != Some("measured")
                    || !merged
                        .get("denominator")
                        .is_some_and(|d| d.as_i64().unwrap_or(0) > 0)
                {
                    merged["state"] = if merged
                        .get("denominator")
                        .is_some_and(|d| d.as_i64().unwrap_or(0) > 0)
                    {
                        json!("partial")
                    } else {
                        json!("unknown")
                    };
                    merged["reason"] = if merged
                        .get("denominator")
                        .is_some_and(|d| d.as_i64().unwrap_or(0) > 0)
                    {
                        json!("one or more site checklists report a partial or unknown state")
                    } else {
                        json!("one or more site checklists cannot justify the denominator")
                    };
                }
            }
            axes.insert(name.clone(), merged);
        }
        result["coverage"] = Value::Object(axes);
        result["exclusions"] = Value::Array(
            sites
                .iter()
                .flat_map(|site| {
                    site["checklist"]
                        .get("exclusions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|entry| {
                            let mut entry = entry.clone();
                            entry["id"] = json!(format!(
                                "site:{}/{}",
                                site["project_uuid"].as_str().unwrap_or_default(),
                                entry["id"].as_str().unwrap_or_default()
                            ));
                            entry
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
        );
    }
    if let Some(by_kind) = primary.get("by_kind").and_then(Value::as_object) {
        result["by_kind"] = Value::Object(
            by_kind
                .keys()
                .map(|kind| {
                    let total: i64 = sites
                        .iter()
                        .map(|s| {
                            s["checklist"]["by_kind"][kind]["total"]
                                .as_i64()
                                .unwrap_or(0)
                        })
                        .sum();
                    let complete: i64 = sites
                        .iter()
                        .map(|s| {
                            s["checklist"]["by_kind"][kind]["complete"]
                                .as_i64()
                                .unwrap_or(0)
                        })
                        .sum();
                    (kind.clone(), json!({"total": total, "complete": complete}))
                })
                .collect(),
        );
    }
    result["items"] = Value::Array(
        sites
            .iter()
            .flat_map(|site| {
                let prefix = format!(
                    "site:{}/",
                    site["project_uuid"].as_str().unwrap_or_default()
                );
                site["checklist"]["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut item| {
                        let local_id = item["id"].as_str().unwrap_or_default().to_owned();
                        item["id"] = json!(format!("{prefix}{local_id}"));
                        item["item_id"] = json!(local_id);
                        item["project_directory"] = site["directory"].clone();
                        item["project_uuid"] = site["project_uuid"].clone();
                        item
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    );
    result["views"] = Value::Object(
        primary
            .get("views")
            .and_then(Value::as_object)
            .map(|views| {
                views
                    .keys()
                    .map(|name| {
                        let ids: Vec<Value> = sites
                            .iter()
                            .flat_map(|site| {
                                let prefix = format!(
                                    "site:{}/",
                                    site["project_uuid"].as_str().unwrap_or_default()
                                );
                                site["checklist"]["views"][name]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|id| {
                                        json!(format!(
                                            "{}{}",
                                            prefix,
                                            id.as_str().unwrap_or_default()
                                        ))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .collect();
                        (name.clone(), Value::Array(ids))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    );
    result["complete"] = json!(sites
        .iter()
        .all(|s| s["checklist"]["complete"].as_bool().unwrap_or(false)));
    result["counts_known"] = json!(true);
    Ok(result)
}

/// `resolve_item_scope`: resolve declared qualified site rows; unqualified IDs stay local.
pub fn resolve_item_scope(
    directory: &Path,
    item_id: &str,
    verify_evidence: bool,
) -> ProjectsResult<(String, String)> {
    if !item_id.starts_with("site:") {
        return Ok((directory.to_string_lossy().to_string(), item_id.to_owned()));
    }
    let (prefix, rest) = match item_id.split_once('/') {
        Some(parts) => parts,
        None => return Err(crate::error::value("qualified item ID has no local ID")),
    };
    let (root, project) = workspace_load(directory)?;
    let requested = prefix.trim_start_matches("site:");
    if requested == project["project_uuid"].as_str().unwrap_or_default() {
        return Ok((root.to_string_lossy().to_string(), rest.to_owned()));
    }
    for child in preparation_status(&root)?
        .get("competitors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if child.get("project_uuid").and_then(Value::as_str) == Some(requested) {
            aggregate_coverage(
                &directory.to_string_lossy(),
                crate::coverage::coverage_status(&root, verify_evidence)?,
                None,
                verify_evidence,
            )?;
            return Ok((
                root.join(child["directory"].as_str().unwrap_or_default())
                    .to_string_lossy()
                    .to_string(),
                rest.to_owned(),
            ));
        }
    }
    Err(crate::error::value(
        "item belongs to an undeclared project scope",
    ))
}
