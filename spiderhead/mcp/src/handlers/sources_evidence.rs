//! Offline evidence and backlink handlers (the reference suite,
//! the reference suite, the reference suite): evidence normalization, the PageSpeed artifact
//! reader and the backlink donor checks.

use std::path::Path;

use serde_json::{json, Value};

use spiderhead_recon::backlink_profile::{build_backlink_profile, export_disavow};
use spiderhead_recon::backlinks::check_backlinks;
use spiderhead_sources::backlink_history::{save_snapshot, snapshot_history};
use spiderhead_sources::evidence_import::{normalize_file, public_provenance};
use spiderhead_sources::provider_artifacts::read_pagespeed_artifact;
use spiderhead_sources::providers::save_local_artifact;

use crate::error::ToolError;

use super::sources_net::{fail, present, str_list};
use super::{kwargs, opt, opt_bool, opt_float, opt_int, opt_str};

const MAX_JSON_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One argument that accepts inline JSON text or a bounded JSON file path
/// (`_json_or_path`).
pub(super) fn json_or_path(value: Option<&Value>, label: &str) -> Result<Option<Value>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Object(_) | Value::Array(_) => Ok(Some(value.clone())),
        Value::String(text) => {
            let text = text.trim();
            if text.starts_with('{') || text.starts_with('[') {
                return serde_json::from_str(text)
                    .map(Some)
                    .map_err(|error| fail(format!("{label} is not valid JSON: {error}")));
            }
            let path = Path::new(text);
            let bounded = path
                .symlink_metadata()
                .map(|meta| meta.is_file() && meta.len() <= MAX_JSON_FILE_BYTES)
                .unwrap_or(false);
            if !bounded {
                return Err(fail(format!(
                    "{label} must be inline JSON or a bounded readable JSON file"
                )));
            }
            let raw = std::fs::read_to_string(path).map_err(fail)?;
            serde_json::from_str(&raw)
                .map(Some)
                .map_err(|error| fail(format!("{label} is not valid JSON: {error}")))
        }
        _ => Err(fail(format!("{label} must be JSON data or a file path"))),
    }
}

/// `evidence_normalize`: normalize one supplied CSV/XLSX/JSON file, offline.
pub fn evidence_normalize(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(file) = present(&args, "file") else {
        return Err(fail("file required"));
    };
    let manifest = json_or_path(opt(&args, "mapping"), "mapping")?;
    let document = normalize_file(
        file,
        manifest.as_ref(),
        opt_str(&args, "sheet"),
        opt_str(&args, "site_origin"),
    )
    .map_err(fail)?;
    let privacy = document["mapping"]["source"]["privacy"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or("supplied")
        .to_owned();
    let artifact = match present(&args, "out_dir") {
        Some(directory) => Some(save_local_artifact(directory, &document).map_err(fail)?),
        None => None,
    };
    let mut result = json!({
        "ok": true,
        "format": "spiderhead.evidence-normalize.v1",
        "privacy": privacy,
        "summary": document["summary"],
        "artifact_reference": artifact,
    });
    if privacy == "restricted" {
        result["provenance"] = public_provenance(&document["provenance"]);
        result["rows_redacted"] = json!(true);
    } else {
        result["document"] = document;
    }
    Ok(result)
}

/// `provider_artifact_read`: a bounded, allowlisted PageSpeed summary page.
pub fn provider_artifact_read(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let null = Value::Null;
    read_pagespeed_artifact(
        opt(&args, "artifact_ref").unwrap_or(&null),
        opt(&args, "artifact_dir").unwrap_or(&null),
        opt(&args, "offset"),
        opt(&args, "limit"),
    )
    .map_err(fail)
}

/// `backlinks_check`: do the donor pages link to the target, and how.
pub fn backlinks_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(target) = present(&args, "target") else {
        return Err(fail("target required"));
    };
    let donors = str_list(&args, "donors").unwrap_or_default();
    if donors.is_empty() {
        return Err(fail("donors[] required"));
    }
    let concurrency = opt_int(&args, "concurrency").unwrap_or(3);
    Ok(check_backlinks(target, &donors, concurrency, 15.0))
}

/// `backlinks_disavow`: the explicit disavow selection, never submitted.
pub fn backlinks_disavow(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    export_disavow(
        str_list(&args, "domains").as_deref(),
        str_list(&args, "urls").as_deref(),
        opt_str(&args, "output"),
    )
    .map_err(fail)
}

/// `backlinks_profile`: merge explicit backlink evidence into one profile and
/// optionally retain a private history snapshot. Provider `sources` entries are
/// not collected here; evidence and imports supplied by the caller are.
pub fn backlinks_profile(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(domain) = present(&args, "domain") else {
        return Err(fail("domain is required"));
    };
    let normalized = spiderhead_recon::net_client::normalize_domain(domain);
    if normalized.is_empty() {
        return Err(fail("domain must be a valid hostname or HTTP(S) URL"));
    }
    if opt_bool(&args, "history_only").unwrap_or(false) {
        return snapshot_history(
            opt(&args, "db"),
            opt(&args, "project"),
            Some(&json!(normalized)),
            opt(&args, "limit"),
            opt(&args, "offset"),
        )
        .map_err(fail);
    }
    if opt_str(&args, "db").is_some() && opt_str(&args, "project").is_some() {
        return Err(fail("pass db or project, not both"));
    }
    if opt(&args, "sources").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        return Err(fail(
            "backlinks-profile sources[] collection is not available in this build",
        ));
    }
    let list = |name: &str| opt(&args, name).and_then(Value::as_array).cloned();
    let imports = list("imports");
    let evidence = list("evidence");
    let failure: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let sink = |host: &str,
                rows: &[Value],
                coverage: &[Value],
                edges: &[Value],
                db: Option<&str>,
                project: Option<&str>,
                observed_at: &str| {
        let saved = save_snapshot(
            &json!(host),
            &json!(rows),
            &json!(coverage),
            &json!(edges),
            db.map(|value| json!(value)).as_ref(),
            project.map(|value| json!(value)).as_ref(),
            &json!(observed_at),
        );
        saved.unwrap_or_else(|error| {
            *failure.borrow_mut() = Some(error.to_string());
            Value::Null
        })
    };
    let profile = build_backlink_profile(
        domain,
        imports.as_deref(),
        evidence.as_deref(),
        opt_str(&args, "db"),
        opt_str(&args, "project"),
        opt_str(&args, "observed_at"),
        opt_str(&args, "state"),
        opt_str(&args, "follow"),
        opt_str(&args, "target_prefix"),
        opt_float(&args, "min_spam_score"),
        opt_float(&args, "max_spam_score"),
        opt_str(&args, "first_seen_from"),
        opt_str(&args, "first_seen_to"),
        opt_bool(&args, "one_per_domain").unwrap_or(false),
        opt(&args, "target_status"),
        opt_int(&args, "limit").unwrap_or(1000),
        opt_int(&args, "offset").unwrap_or(0),
        Some(&sink),
    )
    .map_err(fail)?;
    if let Some(message) = failure.into_inner() {
        return Err(fail(message));
    }
    Ok(profile)
}
