//! Handlers that act on retained scans: reads over history and navigation,
//! and the explicit scan-lifecycle actions (pin, prune, snapshot, requeue,
//! import, archive, sitemap generation).

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_store::history::{pin_scan, prune_apply, prune_preview, snapshot_scan};
use spiderhead_store::scan_artifact::open_scan;

use crate::error::ToolError;

use super::{failed, missing_option, need_str, opt, opt_bool, opt_int, opt_str};

/// A required path-like string argument; empty text is refused like a missing one.
fn path_arg<'a>(
    args: &'a Map<String, Value>,
    name: &str,
    label: &str,
    command: &str,
    accepted: &[&str],
) -> Result<&'a str, ToolError> {
    let value = need_str(args, name, command, accepted)?;
    if value.is_empty() {
        return Err(ToolError::Failed(format!("{label} is required")));
    }
    Ok(value)
}

/// `scan_rendered_routes`: stored route observations, never crawling or rendering.
pub fn scan_rendered_routes(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let path = path_arg(
        &args,
        "input_path",
        "input",
        "scan-rendered-routes",
        &["input_path"],
    )?;
    let con = open_scan(Path::new(path), false, None).map_err(failed)?;
    spiderhead_store::rendered_routes::read(&con)
        .map(Value::Object)
        .map_err(failed)
}

/// `scan_snapshot`: copy a scan (and its audit companion) as a validated pair.
pub fn scan_snapshot(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 2] = ["input_path", "out"];
    let input = path_arg(&args, "input_path", "input", "scan-snapshot", &ACCEPTED)?;
    let out = path_arg(&args, "out", "out", "scan-snapshot", &ACCEPTED)?;
    let snapshot = snapshot_scan(Path::new(input), Path::new(out)).map_err(failed)?;
    Ok(json!({ "snapshot": snapshot }))
}

/// `scan_pin`: protect or release one scan from pruning.
pub fn scan_pin(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input = path_arg(
        &args,
        "input_path",
        "input",
        "scan-pin",
        &["input_path", "pinned"],
    )?;
    let pinned = match opt(&args, "pinned") {
        None => true,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(ToolError::Failed("pinned must be a boolean".into())),
    };
    pin_scan(Path::new(input), pinned).map_err(failed)?;
    Ok(json!({ "input": input, "pinned": pinned }))
}

/// The reviewed prune plan: an object, or a JSON file holding one (or the preview envelope).
fn prune_plan(value: Option<&Value>) -> Result<Map<String, Value>, ToolError> {
    const LIMIT: usize = 64 * 1024 * 1024;
    let refused =
        || ToolError::Failed("prune --apply requires a reviewed plan object or JSON file".into());
    let mut value = value.cloned();
    if let Some(Value::String(path)) = &value {
        if !path.is_empty() {
            let raw = std::fs::read(path).map_err(|e| ToolError::Failed(e.to_string()))?;
            if raw.len() > LIMIT {
                return Err(ToolError::Failed("prune plan exceeds 64 MiB".into()));
            }
            value =
                Some(serde_json::from_slice(&raw).map_err(|e| ToolError::Failed(e.to_string()))?);
        }
    }
    let Some(Value::Object(mut object)) = value else {
        return Err(refused());
    };
    let envelope = object.len() == 2
        && object.get("applied") == Some(&Value::Bool(false))
        && object.contains_key("plan");
    if envelope {
        match object.remove("plan") {
            Some(Value::Object(plan)) => return Ok(plan),
            _ => return Err(refused()),
        }
    }
    Ok(object)
}

/// A result map in the documented key order (the store keeps sorted maps).
fn in_order(map: std::collections::BTreeMap<String, Value>, order: &[&str]) -> Value {
    let mut map = map;
    let mut out = Map::new();
    for key in order {
        if let Some(value) = map.remove(*key) {
            out.insert((*key).to_owned(), value);
        }
    }
    out.extend(map);
    Value::Object(out)
}

/// `scan_prune`: preview a retention plan, or apply a reviewed one.
pub fn scan_prune(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let mut directory = opt_str(&args, "directory").map(str::to_owned);
    if let Some(project) = opt_str(&args, "project") {
        let opened = spiderhead_projects::workspace::open_project(Path::new(project), None)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        if directory.is_none() {
            let root = opened["path"].as_str().unwrap_or_default();
            directory = Some(format!("{root}/scans"));
        }
    }
    let Some(directory) = directory else {
        return Err(ToolError::Failed("directory or project is required".into()));
    };
    if directory.is_empty() {
        return Err(ToolError::Failed("directory is required".into()));
    }
    let apply = match opt(&args, "apply") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(ToolError::Failed("apply must be a boolean".into())),
    };
    if apply && spiderhead_projects::action_log::current_interface() == "mcp" {
        // Deleting scans is owner-only over MCP; the refusal is journaled first.
        if let Some(project) = opt_str(&args, "project") {
            let _ = spiderhead_projects::action_log::record_interface_action(
                Path::new(project),
                "scan.delete",
                "refused",
                "Owner-only action is unavailable without a trusted MCP principal",
                None,
                None,
            );
        }
        spiderhead_projects::permissions::require_mcp_allowed("scan.delete")
            .map_err(|e| ToolError::Failed(e.to_string()))?;
    }
    let root = Path::new(&directory);
    if !apply {
        if opt(&args, "plan").is_some() {
            return Err(ToolError::Failed(
                "a prune plan is only accepted with apply=true".into(),
            ));
        }
        let plan = prune_preview(
            root,
            opt_int(&args, "older_than_days").unwrap_or(30),
            opt_int(&args, "keep_newest").unwrap_or(5),
        )
        .map_err(failed)?;
        return Ok(json!({ "applied": false, "plan": plan }));
    }
    let removed = prune_apply(root, &prune_plan(opt(&args, "plan"))?).map_err(failed)?;
    Ok(json!({ "applied": true, "removed": removed }))
}

/// `scan_config_diff`: field-by-field diff of two scans' recorded configurations.
pub fn scan_config_diff(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 2] = ["before_path", "after_path"];
    let before = need_str(&args, "before_path", "scan-config-diff", &ACCEPTED)?;
    let after = need_str(&args, "after_path", "scan-config-diff", &ACCEPTED)?;
    let diff = spiderhead_store::config_diff::scan_config_diff(Path::new(before), Path::new(after))
        .map_err(failed)?;
    Ok(Value::Object(diff.into_iter().collect()))
}

/// `scan_body_diff`: compare one URL's retained body across two scans.
pub fn scan_body_diff(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 10] = [
        "left",
        "right",
        "url",
        "variant_key",
        "representation",
        "text",
        "max_bytes",
        "max_lines",
        "left_representation",
        "right_representation",
    ];
    let left = path_arg(&args, "left", "left", "scan-body-diff", &ACCEPTED)?;
    let right = path_arg(&args, "right", "right", "scan-body-diff", &ACCEPTED)?;
    let url = path_arg(&args, "url", "url", "scan-body-diff", &ACCEPTED)?;
    let left_con = open_scan(Path::new(left), false, None).map_err(failed)?;
    let right_con = open_scan(Path::new(right), false, None).map_err(failed)?;
    spiderhead_store::body_diff::body_diff(
        &left_con,
        &right_con,
        url,
        opt_str(&args, "variant_key"),
        opt_str(&args, "representation").unwrap_or("static"),
        opt_bool(&args, "text").unwrap_or(false),
        opt_int(&args, "max_bytes").unwrap_or(5 * 1024 * 1024),
        opt_int(&args, "max_lines").unwrap_or(10_000),
        opt_str(&args, "left_representation"),
        opt_str(&args, "right_representation"),
    )
    .map(Value::Object)
    .map_err(failed)
}

/// `scan_navigation`: bounded observed navigation evidence of retained documents.
pub fn scan_navigation(args: &Value) -> Result<Value, ToolError> {
    const ACCEPTED: [&str; 4] = ["input_path", "document_id", "limit", "offset"];
    const BYTE_LIMIT: usize = 2 * 1024 * 1024;
    let args = super::kwargs(args);
    let input = need_str(&args, "input_path", "scan-navigation", &ACCEPTED)?;
    let limit = opt(&args, "limit").map_or(Some(100), Value::as_i64);
    let limit = match limit {
        Some(n) if (1..=1000).contains(&n) => n,
        _ => return Err(ToolError::Failed("limit must be between 1 and 1000".into())),
    };
    let offset = match opt(&args, "offset").map_or(Some(0), Value::as_i64) {
        Some(n) if n >= 0 => n,
        _ => {
            return Err(ToolError::Failed(
                "offset must be a nonnegative integer".into(),
            ))
        }
    };
    let document_id = match opt(&args, "document_id") {
        None => None,
        Some(value) => match value.as_i64() {
            Some(n) if n >= 1 => Some(n),
            _ => {
                return Err(ToolError::Failed(
                    "document_id must be a positive integer".into(),
                ))
            }
        },
    };
    let con = open_scan(Path::new(input), false, None).map_err(failed)?;
    let has_documents: bool = con
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='documents'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !has_documents {
        return Ok(json!({
            "ok": true,
            "schema": "spiderhead.navigation-report.v1",
            "state": "unavailable",
            "reason": "scan_has_no_retained_documents",
            "items": [],
            "has_more": false,
        }));
    }
    let unreadable =
        |reason: String| ToolError::Failed(format!("cannot read navigation evidence: {reason}"));
    let ids: Vec<i64> = match document_id {
        Some(id) => vec![id],
        None => {
            let mut statement = con
                .prepare(
                    "SELECT document_id FROM documents WHERE representation != ? \
                     ORDER BY document_id LIMIT ? OFFSET ?",
                )
                .map_err(|e| unreadable(e.to_string()))?;
            let rows = statement
                .query_map(rusqlite::params!["static", limit + 1, offset], |r| r.get(0))
                .map_err(|e| unreadable(e.to_string()))?;
            rows.filter_map(Result::ok).collect()
        }
    };
    let mut items: Vec<Value> = Vec::new();
    let mut output_bytes = 0usize;
    for number in ids.iter().take(limit as usize) {
        let item = spiderhead_store::bodies::read_document_navigation(&con, *number)
            .map_err(|e| unreadable(super::plain(&e)))?;
        let item = Value::Object(item);
        let size = spiderhead_core::common::python_json(&item).len();
        if output_bytes + size > BYTE_LIMIT {
            break;
        }
        output_bytes += size;
        items.push(item);
    }
    let mut counts: Map<String, Value> = Map::new();
    let mut states: Map<String, Value> = Map::new();
    let mut omitted = 0i64;
    let bump = |map: &mut Map<String, Value>, key: &str| {
        let next = map.get(key).and_then(Value::as_i64).unwrap_or(0) + 1;
        map.insert(key.to_owned(), json!(next));
    };
    for item in &items {
        let navigation = &item["navigation"];
        for event in navigation["events"].as_array().into_iter().flatten() {
            bump(&mut counts, event["kind"].as_str().unwrap_or_default());
        }
        bump(
            &mut states,
            navigation["state"].as_str().unwrap_or_default(),
        );
        omitted += navigation["events_omitted"].as_i64().unwrap_or(0);
    }
    let state_count = |name: &str| states.get(name).and_then(Value::as_i64).unwrap_or(0);
    let state = if items.is_empty() || state_count("unavailable") as usize == items.len() {
        "unavailable"
    } else if state_count("partial") > 0 || state_count("unavailable") > 0 {
        "partial"
    } else {
        "complete"
    };
    Ok(json!({
        "ok": true,
        "schema": "spiderhead.navigation-report.v1",
        "state": state,
        "scope": "selected_retained_documents",
        "items": items,
        "offset": offset,
        "has_more": ids.len() > items.len(),
        "next_offset": offset + items.len() as i64,
        "output_byte_limit": BYTE_LIMIT,
        "event_counts": counts,
        "document_states": states,
        "events_omitted": omitted,
        "interpretation": "Observed navigation within the recorded render window; anchor cause does not prove a human click. Missing or partial capture is not a clean result.",
    }))
}

/// `extraction_presets`: the shipped extraction rule sets.
pub fn extraction_presets(_args: &Value) -> Result<Value, ToolError> {
    Ok(json!({
        "ok": true,
        "presets": spiderhead_parse::extraction_library::available(),
    }))
}

/// `custom_extract_sample`: run declared extractors on supplied HTML; never fetches.
pub fn custom_extract_sample(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 3] = ["html", "extractors", "url"];
    let html = need_str(&args, "html", "custom-extract-sample", &ACCEPTED)?;
    let Some(extractors) = opt(&args, "extractors") else {
        return Err(missing_option(
            "custom-extract-sample",
            "extractors",
            &ACCEPTED,
        ));
    };
    let specs: Vec<spiderhead_parse::custom_extract::ExtractorSpec> =
        serde_json::from_value(extractors.clone()).map_err(|e| ToolError::Failed(e.to_string()))?;
    let report = spiderhead_parse::custom_extract::check_sample(
        html,
        &specs,
        opt_str(&args, "url").unwrap_or(""),
        spiderhead_parse::custom_extract::DEFAULT_TIMEOUT_SECONDS,
    )
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    serde_json::to_value(report).map_err(|e| ToolError::Failed(e.to_string()))
}

/// `scan_archive`: archive retained complete responses as one offline WARC file.
pub fn scan_archive(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 3] = ["input_path", "out", "max_body_bytes"];
    let input = need_str(&args, "input_path", "scan-archive", &ACCEPTED)?;
    let out = need_str(&args, "out", "scan-archive", &ACCEPTED)?;
    Ok(Value::Object(spiderhead_store::scan_archive::build_warc(
        Path::new(input),
        Some(Path::new(out)),
        opt_int(&args, "max_body_bytes").unwrap_or(64 * 1024 * 1024),
    )))
}

/// `scan_sitemap_generate`: XML sitemaps from retained canonical indexable 200 responses.
pub fn scan_sitemap_generate(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 5] = [
        "input_path",
        "out_dir",
        "sitemap_base_url",
        "include_images",
        "exclude",
    ];
    let input = need_str(&args, "input_path", "scan-sitemap-generate", &ACCEPTED)?;
    let out_dir = need_str(&args, "out_dir", "scan-sitemap-generate", &ACCEPTED)?;
    let base = need_str(
        &args,
        "sitemap_base_url",
        "scan-sitemap-generate",
        &ACCEPTED,
    )?;
    let exclude: Option<Vec<String>> =
        opt(&args, "exclude")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
    Ok(Value::Object(
        spiderhead_store::sitemap_generator::generate_sitemaps(
            Path::new(input),
            Path::new(out_dir),
            base,
            opt_bool(&args, "include_images").unwrap_or(false),
            exclude.as_deref(),
            &spiderhead_store::sitemap_generator::StoredDocuments,
        ),
    ))
}

/// `scan_requeue`: upgrade one scan for a selected retry, keeping a verified backup.
pub fn scan_requeue(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 4] = ["input_path", "where", "backup_path", "from_scan"];
    let input = path_arg(&args, "input_path", "input", "scan-requeue", &ACCEPTED)?;
    let predicate = path_arg(&args, "where", "where", "scan-requeue", &ACCEPTED)?;
    let backup = path_arg(
        &args,
        "backup_path",
        "backup_path",
        "scan-requeue",
        &ACCEPTED,
    )?;
    let from_scan = opt_str(&args, "from_scan");
    if from_scan == Some("") {
        return Err(ToolError::Failed("from_scan is required".into()));
    }
    spiderhead_store::retry::requeue_scan(
        Path::new(input),
        predicate,
        Path::new(backup),
        from_scan.map(Path::new),
    )
    .map(|map| in_order(map, &["scan", "backup", "attempt_id", "requeued"]))
    .map_err(failed)
}

/// `scan_import_urls`: add an external URL list through a scan's stored admission policy.
pub fn scan_import_urls(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 3] = ["input_path", "urls_file", "backup_path"];
    let input = path_arg(&args, "input_path", "input", "scan-import-urls", &ACCEPTED)?;
    let urls = path_arg(
        &args,
        "urls_file",
        "urls_file",
        "scan-import-urls",
        &ACCEPTED,
    )?;
    let backup = path_arg(
        &args,
        "backup_path",
        "backup_path",
        "scan-import-urls",
        &ACCEPTED,
    )?;
    spiderhead_store::retry::scan_import_urls(Path::new(input), Path::new(urls), Path::new(backup))
        .map(|map| {
            in_order(
                map,
                &["scan", "backup", "attempt_id", "imported", "rejected"],
            )
        })
        .map_err(failed)
}

/// `scan_fragment_links`: fragment anchors over retained complete HTML/DOM, offline.
pub fn scan_fragment_links(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_store::fragment_links as fragments;
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 5] = ["input_path", "offset", "limit", "state", "representation"];
    let input = need_str(&args, "input_path", "scan-fragment-links", &ACCEPTED)?;
    let limit = opt(&args, "limit").map_or(Some(100), Value::as_i64);
    let offset = opt(&args, "offset").map_or(Some(0), Value::as_i64);
    let (Some(limit), Some(offset)) = (limit, offset) else {
        return Err(ToolError::Failed(
            "limit must be 1..10000 and offset nonnegative".into(),
        ));
    };
    if !(1..=10_000).contains(&limit) || offset < 0 {
        return Err(ToolError::Failed(
            "limit must be 1..10000 and offset nonnegative".into(),
        ));
    }
    let state = opt_str(&args, "state");
    if state.is_some_and(|s| !["resolved", "missing", "skipped"].contains(&s)) {
        return Err(ToolError::Failed(
            "state must be resolved, missing or skipped".into(),
        ));
    }
    let representation = opt_str(&args, "representation");
    if representation.is_some_and(|s| !["static", "rendered", "legacy_fragment"].contains(&s)) {
        return Err(ToolError::Failed(
            "representation must be static, rendered or legacy_fragment".into(),
        ));
    }
    let con = open_scan(Path::new(input), false, None).map_err(failed)?;
    let evaluation = fragments::evaluate(
        &con,
        fragments::DEFAULT_MAX_DECODED_BYTES,
        fragments::MAX_OCCURRENCES,
    )
    .map_err(failed)?;
    let mut result = fragments::paginate(&evaluation, offset, limit, state, representation);
    result.insert("ok".into(), json!(true));
    result.insert(
        "scope".into(),
        json!("retained complete HTML/DOM only; missing or incomplete evidence is skipped by name, never reported as a broken fragment"),
    );
    Ok(Value::Object(result))
}

/// `scan_url_detail`: one exact native URL's bounded retained transport evidence, offline.
pub fn scan_url_detail(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_store::url_detail::{scan_url_detail as core, DetailRequest};
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 10] = [
        "input_path",
        "url",
        "response_offset",
        "response_limit",
        "form_offset",
        "form_limit",
        "max_bytes",
        "body_offset",
        "body_length",
        "max_body_bytes",
    ];
    let input = need_str(&args, "input_path", "scan-url-detail", &ACCEPTED)?;
    let url = need_str(&args, "url", "scan-url-detail", &ACCEPTED)?;
    let request = DetailRequest {
        url,
        response_offset: opt_int(&args, "response_offset").unwrap_or(0),
        response_limit: opt_int(&args, "response_limit").unwrap_or(10),
        form_offset: opt_int(&args, "form_offset").unwrap_or(0),
        form_limit: opt_int(&args, "form_limit").unwrap_or(20),
        max_bytes: opt_int(&args, "max_bytes").unwrap_or(1_048_576),
        body_offset: opt_int(&args, "body_offset").unwrap_or(0),
        body_length: opt_int(&args, "body_length").unwrap_or(4096),
        max_body_bytes: opt_int(&args, "max_body_bytes").unwrap_or(8 * 1024 * 1024),
    };
    Ok(core(input, &request).unwrap_or_else(
        |error| json!({"ok": false, "state": "invalid", "error": super::plain(&error)}),
    ))
}

/// `scan_url_history`: one exact URL's state in each retained scan of a project, newest first.
pub fn scan_url_history(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const ACCEPTED: [&str; 3] = ["project", "url", "limit"];
    let project = need_str(&args, "project", "scan-url-history", &ACCEPTED)?;
    let url = need_str(&args, "url", "scan-url-history", &ACCEPTED)?;
    let (root, _document) = spiderhead_projects::workspace::load(Path::new(project))
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    spiderhead_store::url_history::url_history(
        &root.join("scans"),
        url,
        opt_int(&args, "limit").unwrap_or(50),
    )
    .map(Value::Object)
    .map_err(failed)
}
