use super::*;

/// A finished scan reusable as evidence for a new bounded pass.
pub(super) fn reusable_scan(
    root: &Path,
    target: &str,
    max_urls: Option<i64>,
) -> ProjectsResult<Option<PathBuf>> {
    for row in audited_scans(root, target)? {
        let path = PathBuf::from(row["path"].as_str().unwrap_or_default());
        match max_urls {
            None if is_complete(&row) => return Ok(Some(path)),
            Some(cap)
                if (is_complete(&row)
                    || CAP_STOPS.contains(&row["finish_reason"].as_str().unwrap_or_default()))
                    && scan_url_cap(&path) == Some(cap) =>
            {
                return Ok(Some(path))
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Injected crawl handler: `(project, overrides, approve_large_crawl,
/// producer_build)` -> handler result; `Err` carries the error text.
pub type CrawlSiteTool<'a> = dyn Fn(&str, &Value, bool, Option<&str>) -> Result<Value, String> + 'a;

pub(super) fn save_preparation(root: &Path, state: &mut Value) -> ProjectsResult<()> {
    let previous = read_document(root, "preparation.json")?;
    let revision = previous
        .as_ref()
        .and_then(|p| p["revision"].as_i64())
        .unwrap_or(0);
    state["revision"] = json!(revision + 1);
    write_document(root, "preparation.json", state, None)
}

pub(super) fn falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}

/// Run the bounded native preparation path with injected tools.
///
/// The injected crawl handler performs the bounded collection; everything else
/// (coverage checklist, competitor
/// workspaces, priorities, plan document) is project state.
pub fn prepare_project(
    directory: &Path,
    crawl_site: &CrawlSiteTool<'_>,
    template: Option<&Value>,
    competitors: Option<&Value>,
    approve_large_crawl: bool,
    producer_build: Option<&str>,
    max_urls: Option<i64>,
) -> ProjectsResult<Value> {
    if let Some(cap) = max_urls {
        if !(1..=1_000_000).contains(&cap) {
            return Err(crate::error::value(
                "max_urls must be an integer from 1 to 1000000",
            ));
        }
    }
    let (root, project) = workspace_load(directory)?;
    let policy = project_policy(directory, None, false, None, None)?;
    let limit = policy["policy"]["competitor_limit"].as_i64().unwrap_or(0);
    if let Some(list) = competitors {
        if !list.is_array() || list.as_array().map_or(0, |c| c.len() as i64) > limit {
            return Err(crate::error::value(
                "competitor candidate list exceeds the project limit",
            ));
        }
    }
    let previous_preparation = preparation_status(directory)?;
    let previous_candidates: Vec<Value> = previous_preparation["competitors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| {
            json!({
                "url": row["url"],
                "source": row["source"],
                "observed_at": row["observed_at"],
            })
        })
        .collect();
    let candidate_input = match competitors {
        Some(v) => v.as_array().cloned().unwrap_or_default(),
        None => previous_candidates,
    };
    let candidates: Vec<Value> = candidate_input
        .iter()
        .map(candidate)
        .collect::<ProjectsResult<_>>()?;
    if candidates.len() as i64 > limit {
        return Err(crate::error::value(
            "saved competitor candidate list exceeds the current project limit",
        ));
    }
    let urls: BTreeSet<&str> = candidates
        .iter()
        .map(|row| row["url"].as_str().unwrap_or_default())
        .collect();
    if urls.len() != candidates.len() {
        return Err(crate::error::value("duplicate competitor candidates"));
    }
    if candidates.iter().any(|row| {
        competitor_identity(row["url"].as_str().unwrap_or_default())
            .ok()
            .as_deref()
            == project["site"]["target"].as_str()
    }) {
        return Err(crate::error::value(
            "the primary site cannot be its own competitor candidate",
        ));
    }
    let lock = prepare_lock(&root)?;
    let mut state = json!({
        "format": PREPARATION_FORMAT,
        "project_uuid": project["project_uuid"],
        "revision": 1,
        "started_at": crate::coverage::now(),
        "state": "running",
        "steps": {},
        "competitors": [],
    });
    let result = preparation_body(
        &root,
        &project,
        &policy,
        &mut state,
        crawl_site,
        template,
        &candidates,
        approve_large_crawl,
        producer_build,
        max_urls,
    );
    if let Err(ref e) = result {
        state["state"] = json!("failed");
        let (_, message) = crate::error::py_exception_name(e);
        state["reason"] = json!(if message.is_empty() {
            "preparation interrupted".to_owned()
        } else {
            message
        });
        state["finished_at"] = json!(crate::coverage::now());
        let _ = save_preparation(&root, &mut state);
    }
    let _ = fs::remove_file(&lock);
    result
}

/// Body of `prepare_project`; the outer caller turns any failure into
/// a failed preparation state before surfacing it.
#[allow(clippy::too_many_arguments)]
pub(super) fn preparation_body(
    root: &Path,
    project: &Value,
    policy: &Value,
    state: &mut Value,
    crawl_site: &CrawlSiteTool<'_>,
    template: Option<&Value>,
    candidates: &[Value],
    approve_large_crawl: bool,
    producer_build: Option<&str>,
    max_urls: Option<i64>,
) -> ProjectsResult<Value> {
    crate::coverage::initialize_coverage(root, template)?;
    for (slug, title) in [
        ("sitemap", "Review sitemap discovery and coverage"),
        ("crawl", "Review the bounded preparation crawl"),
        ("competitors", "Review competitor candidates"),
        ("plan", "Review the initial project plan"),
        ("delivery", "Review the final client deliverable"),
    ] {
        let item_id = format!("custom:preparation-{slug}");
        let current = crate::coverage::coverage_status(root, true)?;
        let exists = current["items"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .any(|row| row["id"].as_str() == Some(item_id.as_str()))
            })
            .unwrap_or(false);
        if !exists {
            crate::coverage::update_item(
                root,
                &json!({
                    "id": item_id,
                    "title": title,
                    "execution_kind": if slug == "delivery" { "deliverable" } else { "manual" },
                    "order": 0,
                }),
                current["revision"].as_i64().unwrap_or_default(),
            )?;
        }
    }
    save_preparation(root, state)?;
    let quick = &policy["policy"]["quick_crawl"];
    let pages = max_urls.unwrap_or_else(|| quick["pages"].as_i64().unwrap_or(0));
    let budgets = json!({
        "pages": pages,
        "requests": if max_urls.is_none() {
            quick["requests"].clone()
        } else {
            json!(quick["requests"].as_i64().unwrap_or(0).max(3 * pages))
        },
        "seconds": if max_urls.is_none() {
            quick["seconds"].clone()
        } else {
            json!(quick["seconds"].as_i64().unwrap_or(0).max(2 * pages))
        },
    });
    let mut overrides = policy["policy"]["crawl_overrides"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    overrides.insert("limits.max_urls".to_owned(), budgets["pages"].clone());
    overrides.insert(
        "limits.max_requests".to_owned(),
        budgets["requests"].clone(),
    );
    overrides.insert(
        "limits.max_crawl_seconds".to_owned(),
        budgets["seconds"].clone(),
    );
    overrides.insert("sitemaps.auto_discover".to_owned(), json!(true));
    let target = project["site"]["target"].as_str().unwrap_or_default();
    let reused = reusable_scan(root, target, max_urls)?;
    state["steps"]["crawl"] = json!({
        "state": "running",
        "reason": if reused.is_some() {
            "reusing latest suitable saved scan"
        } else {
            "bounded native crawl requested"
        },
        "budgets": budgets,
    });
    save_preparation(root, state)?;

    // The crawl and evidence block: expected failures are caught here.
    let crawl_block: ProjectsResult<()> = (|| {
        let result = match &reused {
            Some(path) => json!({"ok": true, "scan": path.to_string_lossy()}),
            None => crawl_site(
                &root.to_string_lossy(),
                &Value::Object(overrides),
                approve_large_crawl,
                producer_build,
            )
            .map_err(crate::error::value)?,
        };
        if !result.is_object() || result.get("ok") == Some(&Value::Bool(false)) {
            let text = if result.is_object() {
                result["error"]
                    .as_str()
                    .unwrap_or("crawl was unavailable")
                    .to_owned()
            } else {
                "invalid crawl result".to_owned()
            };
            return Err(crate::error::value(text));
        }
        let scan_value = result
            .get("scan")
            .filter(|v| !falsy(v))
            .cloned()
            .or_else(|| result.get("scan_path").cloned());
        let scan_text = match scan_value {
            Some(Value::Object(m)) => m.get("path").cloned(),
            other => other,
        };
        let Some(scan_str) = scan_text.and_then(|v| v.as_str().map(str::to_owned)) else {
            return Err(crate::error::value(
                "crawl did not return its retained scan path",
            ));
        };
        let source = crate::competitor_scans::resolve_lenient(Path::new(&scan_str));
        if !source.starts_with(root.join("scans")) || !source.is_file() {
            return Err(crate::error::value(
                "crawl result is not a retained project scan",
            ));
        }
        let relative = source
            .strip_prefix(root)
            .map_err(|_| crate::error::value("crawl result is not a retained project scan"))?
            .to_string_lossy()
            .replace('\\', "/");
        let con = spiderhead_store::scan_artifact::open_scan(&source, false, None)
            .map_err(ProjectsError::Store)?;
        let (crawl_partial, scan_uuid, finish_reason): (bool, Value, Value) = con
            .query_row(
                "SELECT crawl_partial, scan_uuid, finish_reason FROM scan WHERE singleton=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?.unwrap_or(0) != 0,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map(|(p, u, f)| (p, json!(u), json!(f)))
            .map_err(|e| ProjectsError::Store(spiderhead_store::error::StoreError::Sqlite(e)))?;
        let (run, coverage, fired) = crate::evidence::audit_facts(&source, &con)?;
        let at_owner_cap =
            max_urls.is_some() && CAP_STOPS.contains(&finish_reason.as_str().unwrap_or_default());
        let partial = crawl_partial && !at_owner_cap;
        state["steps"]["crawl"] = json!({
            "state": if partial { "partial" } else { "run" },
            "artifact": relative,
            "scan_uuid": scan_uuid,
            "partial": partial,
            "reason": if at_owner_cap {
                json!(format!("reached your URL limit ({})", max_urls.unwrap_or_default()))
            } else {
                finish_reason
            },
            "reused": reused.is_some(),
        });
        let sitemap_skips: Vec<Value> = run["checks_skipped"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| {
                row["id"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("SITEMAP_")
            })
            .collect();
        state["steps"]["sitemap"] = json!({
            "state": if sitemap_skips.is_empty() { "run" } else { "partial" },
            "artifact": relative,
            "reason": "saved sitemap coverage; unavailable checks remain explicit",
            "unavailable": sitemap_skips,
        });
        let evidence = crate::crawl_evidence::record_scan(root, &source)?;
        let mut executed: BTreeSet<String> = coverage["checks_silent_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        executed.extend(fired);
        let unavailable: Map<String, Value> = run["checks_skipped"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                row.get("id").and_then(Value::as_str).map(|id| {
                    (
                        id.to_owned(),
                        row.get("reason").cloned().unwrap_or(Value::Null),
                    )
                })
            })
            .collect();
        let mut recording_errors: Vec<Value> =
            evidence["refused"].as_array().cloned().unwrap_or_default();
        let items = crate::coverage::coverage_status(root, true)?["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for item in items {
            if item["kind"].as_str() != Some("check")
                || !falsy(&item["complete"])
                || !item["enabled"].as_bool().unwrap_or(false)
            {
                continue;
            }
            let item_id = item["id"].as_str().unwrap_or_default();
            let check_id = item_id.strip_prefix("check:").unwrap_or(item_id);
            let entry = if executed.contains(check_id) {
                json!({
                    "status": "succeeded",
                    "reason": "Executed in the bounded preparation scan; see recorded measurement scope",
                    "artifact": relative,
                })
            } else if unavailable.contains_key(check_id) {
                json!({
                    "status": "unavailable",
                    "reason": unavailable[check_id].as_str().unwrap_or_default(),
                })
            } else {
                continue;
            };
            let revision = crate::coverage::coverage_status(root, true)?["revision"]
                .as_i64()
                .unwrap_or_default();
            match crate::coverage::record_execution(root, item_id, &entry, revision) {
                Ok(_) => {}
                Err(e @ (ProjectsError::Value(_) | ProjectsError::Json(_))) => {
                    let (_, message) = crate::error::py_exception_name(&e);
                    recording_errors.push(json!({"id": item_id, "reason": message}));
                }
                Err(e) => return Err(e),
            }
        }
        if !recording_errors.is_empty() {
            state["steps"]["crawl"]["recording_gaps"] = Value::Array(recording_errors);
        }
        Ok(())
    })();
    if let Err(e) = crawl_block {
        match e {
            ProjectsError::Value(_) | ProjectsError::Json(_) | ProjectsError::Io(_) => {
                let (_, message) = crate::error::py_exception_name(&e);
                state["steps"]["crawl"] = json!({"state": "not_run", "reason": message});
                state["steps"]["sitemap"] = json!({
                    "state": "not_run",
                    "reason": "crawl source unavailable; sitemap coverage not established",
                });
            }
            other => return Err(other),
        }
    }
    save_preparation(root, state)?;
    if candidates.is_empty() {
        state["steps"]["competitors"] = json!({
            "state": "not_run",
            "reason": "no supplied or authorized competitor source; no competitors invented",
        });
    } else {
        let rows = competitor_workspaces(root, candidates)?;
        state["competitors"] = Value::Array(rows);
        state["steps"]["competitors"] = json!({
            "state": "run",
            "reason": "operator-supplied candidate shortlist; competitiveness is not verified",
            "count": candidates.len(),
        });
    }
    let current = crate::coverage::coverage_status(root, true)?;
    crate::priorities::project_priorities(root, None, true, current["revision"].as_i64(), false)?;
    let plan_name = format!(
        "initial-plan-{}.md",
        &Uuid::new_v4().simple().to_string()[..12]
    );
    state["steps"]["plan"] = json!({
        "state": "run",
        "artifact": format!("reports/{plan_name}"),
        "reason": "initial work plan created; manual and deliverable acceptance remain pending",
    });
    let mut lines = vec![
        "# Initial project plan".to_owned(),
        String::new(),
        format!(
            "Target: {}",
            project["site"]["target"].as_str().unwrap_or_default()
        ),
        String::new(),
        "Preparation observations:".to_owned(),
    ];
    if let Some(steps) = state["steps"].as_object() {
        for (name, row) in steps {
            lines.push(format!(
                "- {name}: {} — {}",
                row["state"].as_str().unwrap_or_default(),
                row["reason"].as_str().unwrap_or_default()
            ));
        }
    }
    lines.extend([
        String::new(),
        "Review the preparation checklist, verify candidate competitors, and use project status for remaining work. This file does not establish a completed audit or client delivery.".to_owned(),
        String::new(),
    ]);
    io(fs::write(
        root.join("reports").join(&plan_name),
        lines.join("\n"),
    ))?;
    let all_run = state["steps"]
        .as_object()
        .map(|steps| {
            steps
                .values()
                .all(|row| row["state"].as_str() == Some("run"))
        })
        .unwrap_or(false);
    state["state"] = json!(if all_run { "prepared" } else { "partial" });
    state["finished_at"] = json!(crate::coverage::now());
    save_preparation(root, state)?;
    let status = crate::workspace::project_status(root, None, true)?;
    Ok(json!({"ok": true, "preparation": state, "status": status}))
}
